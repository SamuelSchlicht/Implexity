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



use std::path::Path;
use std::sync::Mutex;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::py::{sha256_hex, uuid_hex};
use crate::sync::lock;

static LOCK: Mutex<()> = Mutex::new(());
const JOURNAL: &str = "guided_setup.rollback.json";
pub const PATHS: [&str; 2] = ["engineering/problem.json", "optimization_setup.json"];
const MAX: u64 = 96 * 1024 * 1024;

fn verr(m: &str) -> AuthoringError {
    AuthoringError::value("ValueError", m)
}

fn io(e: &std::io::Error) -> AuthoringError {
    AuthoringError::Io(e.to_string())
}

fn sync_dir(path: &Path) -> AResult<()> {
    implexity_io::atomic::fsync_directory(path).map_err(|e| AuthoringError::Io(e.to_string()))
}


pub fn atomic_bytes(path: &Path, content: &[u8]) -> AResult<()> {
    if path.is_symlink() {
        return Err(verr("Setup records must not be symbolic links."));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| io(&e))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let temporary = parent.join(format!("{name}.{}.tmp", uuid_hex()));
    let result = (|| -> AResult<()> {
        use std::io::Write as _;
        let mut f =
            std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e| io(&e))?;
        f.write_all(content).map_err(|e| io(&e))?;
        f.flush().map_err(|e| io(&e))?;
        f.sync_all().map_err(|e| io(&e))?;
        drop(f);
        std::fs::rename(&temporary, path).map_err(|e| io(&e))?;
        sync_dir(parent)
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

fn read_bytes(root: &Path, name: &str) -> AResult<Option<Vec<u8>>> {
    let p = root.join(name);
    if p.is_symlink() || p.parent().is_some_and(Path::is_symlink) {
        return Err(verr("Setup records must not be symbolic links."));
    }
    if p.exists() { Ok(Some(std::fs::read(&p).map_err(|e| io(&e))?)) } else { Ok(None) }
}

fn sha(data: Option<&[u8]>) -> Value {
    data.map_or(Value::Null, |d| Value::from(sha256_hex(d)))
}


pub fn recover(directory: &Path) -> AResult<bool> {
    let _g = lock(&LOCK);
    recover_locked(directory)
}

fn recover_locked(root: &Path) -> AResult<bool> {
    let journal = root.join(JOURNAL);
    if !journal.exists() {
        return Ok(false);
    }
    let meta = std::fs::symlink_metadata(&journal).map_err(|e| io(&e))?;
    if meta.file_type().is_symlink() || meta.len() > MAX {
        return Err(verr("Unsafe or oversized interrupted setup journal."));
    }
    let text = std::fs::read_to_string(&journal).map_err(|e| io(&e))?;
    let record: Value = serde_json::from_str(&text).map_err(|e| verr(&e.to_string()))?;
    let files = record.get("files").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut keys: Vec<&str> = files.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut want = PATHS.to_vec();
    want.sort_unstable();
    if record.get("schema").and_then(Value::as_str) != Some("implexity-guided-setup-rollback/1")
        || keys != want
    {
        return Err(verr("Unrecognized interrupted setup transaction. No files were changed."));
    }
    let mut restore: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    for (name, item) in &files {
        let old = match item.get("old") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(s)
                    .map_err(|_| verr("Interrupted setup journal checksum mismatch."))?,
            ),
            Some(_) => return Err(verr("Interrupted setup journal checksum mismatch.")),
        };
        if sha(old.as_deref()) != item.get("old_sha256").cloned().unwrap_or(Value::Null) {
            return Err(verr("Interrupted setup journal checksum mismatch."));
        }
        let current = sha(read_bytes(root, name)?.as_deref());
        if current != item.get("old_sha256").cloned().unwrap_or(Value::Null)
            && current != item.get("new_sha256").cloned().unwrap_or(Value::Null)
        {
            return Err(verr("Setup record changed outside the interrupted transaction. Recovery refused."));
        }
        restore.push((name.clone(), old));
    }
    for (name, old) in restore {
        let path = root.join(&name);
        match old {
            None => {
                let _ = std::fs::remove_file(&path);
                if let Some(p) = path.parent().filter(|p| p.exists()) {
                    sync_dir(p)?;
                }
            }
            Some(bytes) => atomic_bytes(&path, &bytes)?,
        }
    }
    std::fs::remove_file(&journal).map_err(|e| io(&e))?;
    sync_dir(root)?;
    Ok(true)
}


pub fn commit(directory: &Path, problem_record: &Value, setup_record: &Value) -> AResult<()> {
    let _g = lock(&LOCK);
    recover_locked(directory)?;
    std::fs::create_dir_all(directory).map_err(|e| io(&e))?;
    let new: Vec<Vec<u8>> = [problem_record, setup_record]
        .iter()
        .map(|v| format!("{}\n", crate::py::canonical_ascii(v)).into_bytes())
        .collect();
    let mut files = Map::new();
    for (name, n) in PATHS.iter().zip(&new) {
        let old = read_bytes(directory, name)?;
        files.insert(
            (*name).to_string(),
            json!({"old": old.as_ref().map(|o| base64::engine::general_purpose::STANDARD.encode(o)),
                "old_sha256": sha(old.as_deref()), "new_sha256": sha(Some(n))}),
        );
    }
    let wire = implexity_core::json::dumps(
        &json!({"schema": "implexity-guided-setup-rollback/1", "files": files}),
        &implexity_core::json::DumpOptions::default(),
    );
    if wire.len() as u64 > MAX {
        return Err(verr("Combined setup exceeds the transaction journal limit."));
    }
    let journal = directory.join(JOURNAL);
    atomic_bytes(&journal, wire.as_bytes())?;
    let result = (|| -> AResult<()> {
        for (name, n) in PATHS.iter().zip(&new) {
            atomic_bytes(&directory.join(name), n)?;
        }
        std::fs::remove_file(&journal).map_err(|e| io(&e))?;
        sync_dir(directory)
    })();
    if let Err(e) = result {
        recover_locked(directory)?;
        return Err(e);
    }
    Ok(())
}
