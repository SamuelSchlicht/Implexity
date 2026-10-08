// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path};

use implexity_io::fsguard::{self, FileStat};

use serde_json::{Map, Value, json};

use crate::canonical::{canonical_sha256, is_digest, sha256_hex};
use crate::worker_runtime_profile::{
    BOOTSTRAP_TARGETS, BootstrapEvidence, WorkerRuntimeProfileError, bootstrap_worker,
};

pub const MAXIMUM_SOURCE_BYTES: u64 = 256 * 1024 * 1024;

const MANIFEST_KEYS: [&str; 16] = [
    "aggregate_sha256",
    "excluded_name_prefixes",
    "excluded_names",
    "excluded_parts",
    "excluded_prefixes",
    "excluded_suffixes",
    "file_count",
    "files",
    "generated_utc",
    "product",
    "root",
    "schema",
    "schema_compatibility_aliases",
    "self_excluded",
    "truth_status",
    "version",
];

type BootResult<T> = Result<T, WorkerRuntimeProfileError>;

fn fail<T>(message: impl Into<String>) -> BootResult<T> {
    Err(WorkerRuntimeProfileError::Profile(message.into()))
}

fn digest<'a>(value: Option<&'a str>, label: &str) -> BootResult<&'a str> {
    match value {
        Some(v) if is_digest(v) => Ok(v),
        _ => fail(format!("{label} is not a lowercase SHA-256 digest")),
    }
}

fn same_identity(a: &FileStat, b: &FileStat) -> bool {
    a.data_identity() == b.data_identity() && a.ctime == b.ctime
}


pub fn read_regular_bytes(path: &Path, label: &str, maximum_bytes: u64) -> BootResult<Vec<u8>> {
    let Ok(link) = fsguard::stat_nofollow(path) else {
        return fail(format!("{label} is unavailable"));
    };
    if link.is_symlink() {
        return fail(format!("{label} is unavailable"));
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return fail(format!("{label} is unavailable"));
    };
    let Ok(before) = fsguard::stat_file(&file) else {
        return fail(format!("{label} is unavailable"));
    };
    if !before.is_file() || before.nlink != 1 || before.size > maximum_bytes || !before.same_object(&link) {
        return fail(format!("{label} is not one bounded regular file"));
    }
    let mut bytes = Vec::new();
    let read = (&mut file).take(maximum_bytes + 1).read_to_end(&mut bytes);
    if read.is_err() {
        return fail(format!("{label} is unavailable"));
    }
    if bytes.len() as u64 > maximum_bytes {
        return fail(format!("{label} exceeds its byte bound"));
    }
    let Ok(after) = fsguard::stat_file(&file) else {
        return fail(format!("{label} is unavailable"));
    };
    if !same_identity(&before, &after) || bytes.len() as u64 != before.size {
        return fail(format!("{label} changed during its single read"));
    }
    Ok(bytes)
}

fn strict_manifest(raw: &[u8]) -> BootResult<Map<String, Value>> {
    let Ok(text) = std::str::from_utf8(raw) else {
        return fail("worker source manifest is not strict UTF-8 JSON");
    };
    match implexity_core::json::parse_strict(text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => fail("worker source manifest is not an object"),
        Err(e) => {
            let message = e.to_string();
            if message.contains("duplicate") {
                fail("worker source manifest contains a duplicate key")
            } else if message.to_ascii_lowercase().contains("nan") || message.contains("finite") {
                fail("worker source manifest contains a nonfinite number")
            } else {
                fail("worker source manifest is not strict UTF-8 JSON")
            }
        }
    }
}

fn canonical_relative(relative: &str) -> bool {
    !relative.is_empty()
        && !relative.starts_with('/')
        && relative.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn discover_service_sources(root: &Path, dir: &Path, out: &mut BTreeSet<String>) -> BootResult<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return fail("worker service source inventory is unsafe");
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return fail("worker service source inventory is unsafe");
        };
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return fail("worker service source inventory is unsafe");
        };
        let is_py = path.extension().is_some_and(|e| e == "py");
        if meta.is_dir() {
            if is_py {
                return fail("worker service source inventory is unsafe");
            }
            discover_service_sources(root, &path, out)?;
        } else if is_py {
            if !meta.is_file() {
                return fail("worker service source inventory is unsafe");
            }
            let relative = path.strip_prefix(root).ok().map(|p| {
                p.components()
                    .filter_map(|c| match c {
                        Component::Normal(s) => s.to_str(),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/")
            });
            let Some(relative) = relative else {
                return fail("worker service source inventory is unsafe");
            };
            out.insert(relative);
        }
    }
    Ok(())
}


#[allow(clippy::too_many_lines)]
pub fn verify_source_before_import(
    root: Option<&Path>,
    expected_manifest_sha256: &str,
    target: &str,
) -> BootResult<()> {
    let expected_sha = digest(Some(expected_manifest_sha256), "worker source manifest")?;
    let Some(root) = root.and_then(|r| std::fs::canonicalize(r).ok()) else {
        return fail("source manifest is unavailable");
    };
    let raw =
        read_regular_bytes(&root.join("SOURCE_MANIFEST.json"), "source manifest", MAXIMUM_SOURCE_BYTES)?;
    if sha256_hex(&raw) != expected_sha {
        return fail("worker source manifest identity drifted");
    }
    let manifest = strict_manifest(&raw)?;
    let rows = manifest.get("files").and_then(Value::as_array);
    let keys: BTreeSet<&str> = manifest.keys().map(String::as_str).collect();
    let header_ok = keys == MANIFEST_KEYS.into_iter().collect()
        && manifest.get("schema").and_then(Value::as_str) == Some("implexity-source-manifest/2")
        && manifest.get("product").and_then(Value::as_str) == Some("Implexity")
        && manifest.get("root").and_then(Value::as_str) == Some("Implexity")
        && manifest.get("truth_status").and_then(Value::as_str) == Some("exact_source_inventory")
        && rows.is_some_and(|r| {
            !r.is_empty()
                && manifest
                    .get("file_count")
                    .is_some_and(|c| (c.is_i64() || c.is_u64()) && c.as_u64() == Some(r.len() as u64))
        });
    let Some(rows) = rows.filter(|_| header_ok) else {
        return fail("worker source manifest header drifted");
    };
    let mut checked_rows = Vec::with_capacity(rows.len());
    let mut listed = BTreeSet::new();
    let mut previous = String::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(row) = row.as_object().filter(|r| {
            r.len() == 3 && r.contains_key("path") && r.contains_key("bytes") && r.contains_key("sha256")
        }) else {
            return fail(format!("worker source manifest row {index} is malformed"));
        };
        let relative = row.get("path").and_then(Value::as_str).unwrap_or_default();
        if !canonical_relative(relative) || (!previous.is_empty() && relative <= previous.as_str()) {
            return fail("worker source manifest path order drifted");
        }
        previous = relative.to_string();
        let size = row.get("bytes");
        let row_digest =
            digest(row.get("sha256").and_then(Value::as_str), &format!("source row {relative}"))?;
        let Some(size) = size.filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_u64) else {
            return fail("worker source manifest size is malformed");
        };
        checked_rows.push(json!({"path": relative, "bytes": size, "sha256": row_digest}));
        #[allow(clippy::case_sensitive_file_extension_comparisons, reason = "Python `str.endswith`")]
        let service_source = relative.starts_with("service/implexity/") && relative.ends_with(".py");
        if service_source {
            let leaf = read_regular_bytes(
                &root.join(relative),
                &format!("service source {relative}"),
                MAXIMUM_SOURCE_BYTES,
            )?;
            if leaf.len() as u64 != size || sha256_hex(&leaf) != row_digest {
                return fail(format!("worker service source drifted: {relative}"));
            }
            listed.insert(relative.to_string());
        }
    }
    let aggregate =
        digest(manifest.get("aggregate_sha256").and_then(Value::as_str), "source manifest aggregate")?;
    if canonical_sha256(&Value::Array(checked_rows)) != aggregate {
        return fail("worker source manifest aggregate drifted");
    }
    let service_root = root.join("service").join("implexity");
    if !std::fs::symlink_metadata(&service_root).is_ok_and(|m| m.is_dir()) {
        return fail("worker service source root is unavailable");
    }
    let mut discovered = BTreeSet::new();
    discover_service_sources(&root, &service_root, &mut discovered)?;
    if discovered != listed {
        return fail("worker service source inventory drifted");
    }
    let required = [
        "service/implexity/cae/worker_preimport_bootstrap.py".to_string(),
        "service/implexity/cae/worker_runtime_profile.py".to_string(),
        "service/implexity/cae/exact_acceleration_qualification.py".to_string(),
        "service/implexity/cae/exact_acceleration_promotion_registry.py".to_string(),
        format!("service/{}.py", target.replace('.', "/")),
    ];
    if !required.iter().all(|r| listed.contains(r)) {
        return fail("worker source manifest omits a bootstrap dependency");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct BootstrapArgs<'a> {
    pub target: &'a str,
    pub runtime_policy: &'a str,
    pub runtime_sha256: &'a str,
    pub source_manifest_sha256: &'a str,
}


pub fn main(args: &BootstrapArgs<'_>, source_root: Option<&Path>) -> BootResult<BootstrapEvidence> {
    if !BOOTSTRAP_TARGETS.contains(&args.target) {
        return fail("worker pre-import bootstrap target is not closed");
    }
    digest(Some(args.runtime_sha256), "worker runtime profile")?;
    verify_source_before_import(source_root, args.source_manifest_sha256, args.target)?;
    bootstrap_worker(args.target, args.runtime_policy, args.runtime_sha256)
}

#[must_use]
pub fn evidence_environment(evidence: &BootstrapEvidence) -> [(&'static str, String); 3] {
    [
        ("IMPLEXITY_WORKER_PREIMPORT_BOOTSTRAP_SCHEMA", evidence.schema.clone()),
        ("IMPLEXITY_WORKER_PREIMPORT_VALIDATED_PROFILE_SHA256", evidence.validated_profile_sha256.clone()),
        (
            "IMPLEXITY_WORKER_PREIMPORT_NUMERICAL_MODULES_JSON",
            serde_json::to_string(&evidence.numerical_modules).unwrap_or_else(|_| "[]".into()),
        ),
    ]
}

