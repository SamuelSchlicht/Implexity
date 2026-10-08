// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{PYTHON_COMPATIBILITY_VERSION, RUST_VERSION};

fn text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn nonnegative(path: &Path) -> Option<u128> {
    let t = text(path)?;
    let t = t.trim();
    if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) { t.parse().ok() } else { None }
}

fn unescape_mount(value: &str) -> String {

    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 4 <= bytes.len()
            && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b))
        {
            let code = u32::from(bytes[i + 1] - b'0') * 64
                + u32::from(bytes[i + 2] - b'0') * 8
                + u32::from(bytes[i + 3] - b'0');
            if let Ok(b) = u8::try_from(code) {
                out.push(b);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn has_parent_dir(path: &str) -> bool {
    path.split('/').any(|p| p == "..")
}

#[must_use]
pub fn cgroup_directories(proc_root: &Path) -> Vec<(String, PathBuf)> {
    let membership = text(&proc_root.join("self/cgroup")).unwrap_or_default();
    let mounts = text(&proc_root.join("self/mountinfo")).unwrap_or_default();
    let mut entries: Vec<(&str, String)> = Vec::new();
    for line in membership.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() != 3 {
            continue;
        }
        if parts[0] == "0" && parts[1].is_empty() {
            entries.push(("v2", parts[2].to_string()));
        } else if parts[1].split(',').any(|c| c == "memory") {
            entries.push(("v1", parts[2].to_string()));
        }
    }
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for line in mounts.lines() {
        let Some((left, right)) = line.split_once(" - ") else { continue };
        let a: Vec<&str> = left.split_whitespace().collect();
        let b: Vec<&str> = right.split_whitespace().collect();
        if a.len() < 6 || b.len() < 3 {
            continue;
        }
        let kind = if b[0] == "cgroup2" {
            "v2"
        } else if b[0] == "cgroup" && b[2].split(',').any(|c| c == "memory") {
            "v1"
        } else {
            continue;
        };
        let root = unescape_mount(a[3]);
        let mount = unescape_mount(a[4]);
        if !root.starts_with('/')
            || !mount.starts_with('/')
            || has_parent_dir(&root)
            || has_parent_dir(&mount)
        {
            continue;
        }
        for (member_kind, member) in &entries {
            if *member_kind != kind || !member.starts_with('/') || has_parent_dir(member) {
                continue;
            }
            let root_norm = root.trim_end_matches('/');
            let suffix: Option<String> = if root_norm.is_empty() {
                Some(member.trim_start_matches('/').to_string())
            } else if member == root_norm {
                Some(String::new())
            } else if let Some(rest) = member.strip_prefix(&format!("{root_norm}/")) {
                Some(rest.to_string())
            } else if member == "/" {
                Some(String::new())
            } else {
                None
            };
            let Some(suffix) = suffix else { continue };
            let mount_path = PathBuf::from(&mount);
            let mut current = mount_path.clone();
            for part in suffix.split('/').filter(|p| !p.is_empty()) {
                current.push(part);
            }
            loop {
                let pair = (kind.to_string(), current.clone());
                if !found.contains(&pair) {
                    found.push(pair);
                }
                if current == mount_path {
                    break;
                }
                if !current.pop() {
                    break;
                }
            }
        }
    }
    found
}

#[must_use]
pub fn memory_observation(proc_root: &Path) -> Value {
    let mut total: Option<u128> = None;
    let mut available: Option<u128> = None;
    let mut method = "unavailable";
    for line in text(&proc_root.join("meminfo")).unwrap_or_default().lines() {
        for (key, slot) in [("MemTotal", &mut total), ("MemAvailable", &mut available)] {
            if let Some(rest) = line.strip_prefix(key).and_then(|r| r.strip_prefix(':')) {
                let rest = rest.trim();
                if let Some(kb) = rest.strip_suffix("kB").map(str::trim)
                    && !kb.is_empty()
                    && kb.bytes().all(|b| b.is_ascii_digit())
                {
                    *slot = kb.parse::<u128>().ok().map(|v| v * 1024);
                }
            }
        }
    }
    if total.is_some() || available.is_some() {
        method = "proc_meminfo";
    }
    let mut rows: Vec<Value> = Vec::new();
    let mut limits: Vec<u128> = Vec::new();
    let mut room: Vec<u128> = Vec::new();
    let mut incomplete = false;
    for (kind, folder) in cgroup_directories(proc_root) {
        let (limit_name, usage_name) = if kind == "v2" {
            ("memory.max", "memory.current")
        } else {
            ("memory.limit_in_bytes", "memory.usage_in_bytes")
        };
        let raw_limit = text(&folder.join(limit_name));
        let mut limit = nonnegative(&folder.join(limit_name));
        let usage = nonnegative(&folder.join(usage_name));
        if raw_limit.is_none() && usage.is_none() {
            continue;
        }
        let unlimited = (kind == "v2" && raw_limit.as_deref().is_some_and(|r| r.trim() == "max"))
            || (kind == "v1" && limit.is_some_and(|l| l >= (1u128 << 60)));
        let limit_known = unlimited || limit.is_some();
        if unlimited {
            limit = None;
        }
        let remaining = match (limit, usage) {
            (Some(l), Some(u)) => Some(l.saturating_sub(u)),
            _ => None,
        };
        if let Some(l) = limit {
            limits.push(l);
        }
        if let Some(r) = remaining {
            room.push(r);
        }
        if !limit_known || (limit.is_some() && usage.is_none()) {
            incomplete = true;
        }
        rows.push(json!({
            "version": kind, "path": folder.display().to_string(), "limit_bytes": limit, "usage_bytes": usage,
            "remaining_bytes": remaining, "limit_readable": raw_limit.is_some(), "limit_known": limit_known,
            "unlimited": unlimited,
        }));
    }
    let capacities: Vec<u128> = total.into_iter().chain(limits).collect();
    let availability: Vec<u128> = available.into_iter().chain(room).collect();
    json!({
        "schema": "implexity-memory-observation/1",
        "host_method": method,
        "host_total_bytes": total,
        "host_available_bytes": available,
        "cgroups": rows,
        "effective_capacity_bytes": capacities.iter().min(),
        "effective_available_bytes": if incomplete { None } else { availability.iter().min().copied() },
        "complete_for_observed_limits": !incomplete,
        "job_allocation_guaranteed": false,
    })
}

#[must_use]
pub fn platform_system() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "Darwin",
        "freebsd" => "FreeBSD",
        other => other,
    }
}

#[must_use]
pub fn platform_machine() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "AMD64",
        ("windows", "aarch64") => "ARM64",
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    }
}



#[must_use]
pub fn inspect_runtime_environment() -> Value {
    json!({
        "schema": "implexity-runtime-environment/1",
        "version": PYTHON_COMPATIBILITY_VERSION,
        "rust_build_version": RUST_VERSION,
        "python": Value::Null,
        "platform": platform_system(),
        "machine": platform_machine(),
        "dependencies": {},
        "declared_physics_dependencies_match": true,
        "jax_x64": {
            "environment_request": std::env::var("JAX_ENABLE_X64").ok(),
            "process_enabled": true,
            "jax_already_imported": false,
        },
        "memory": memory_observation(Path::new("/proc")),
        "observation_only": true,
        "solver_executed": false,
        "engineering_acceptance": false,
        "scope": "Metadata and live resource observations only. Run doctor --profile physics for import/JIT checks. Neither operation grants model validation or preflight authority.",
    })
}

