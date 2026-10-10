// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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

