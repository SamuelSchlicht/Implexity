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



use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use implexity_core::json::{DumpOptions, canonical, dumps};

use crate::digest::sha256_hex;

pub const SCHEMA: &str = "implexity-source-manifest/2";
pub const MANIFEST_FILE: &str = "SOURCE_MANIFEST.json";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InventoryError(pub String);

fn walk(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) -> Result<(), InventoryError> {
    let rd = std::fs::read_dir(dir).map_err(|e| InventoryError(format!("{}: {e}", dir.display())))?;
    for entry in rd {
        let entry = entry.map_err(|e| InventoryError(e.to_string()))?;
        let path = entry.path();
        let ft = entry.file_type().map_err(|e| InventoryError(e.to_string()))?;
        let matches = path.file_name().is_some_and(|n| n.to_string_lossy().ends_with(suffix));
        if ft.is_symlink() {
            if matches {
                return Err(InventoryError(format!("Source symlink is not supported: {}", path.display())));
            }
        } else if ft.is_dir() {
            walk(&path, suffix, out)?;
        } else if matches && ft.is_file() {
            out.push(path);
        }
    }
    Ok(())
}


pub fn manifest_for(
    root: &Path,
    source_rel: &str,
    suffix: &str,
    scope: &str,
) -> Result<Value, InventoryError> {
    let root = std::fs::canonicalize(root).map_err(|e| InventoryError(format!("{}: {e}", root.display())))?;
    let source: PathBuf = source_rel.split('/').fold(root.clone(), |p, s| p.join(s));
    let meta = std::fs::symlink_metadata(&source);
    if !meta.as_ref().is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
        return Err(InventoryError("Missing regular implementation source directory".into()));
    }
    let mut files = Vec::new();
    walk(&source, suffix, &mut files)?;
    files.sort();
    let mut rows = Vec::new();
    for path in files {
        let raw = std::fs::read(&path).map_err(|e| InventoryError(format!("{}: {e}", path.display())))?;
        let rel = path.strip_prefix(&root).map_err(|e| InventoryError(e.to_string()))?;
        let posix: Vec<String> =
            rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        rows.push(json!({"path": posix.join("/"), "bytes": raw.len(), "sha256": sha256_hex(&raw)}));
    }
    if rows.is_empty() {
        return Err(InventoryError("Empty implementation source inventory".into()));
    }
    let rows = Value::Array(rows);
    let aggregate = sha256_hex(canonical(&rows).as_bytes());
    let count = rows.as_array().map_or(0, Vec::len);
    Ok(json!({
        "schema": SCHEMA, "product": "Implexity", "root": "Implexity",
        "truth_status": "exact_source_inventory", "scope": scope, "file_count": count,
        "aggregate_sha256": aggregate, "files": rows,
    }))
}


pub fn implementation_manifest(root: &Path) -> Result<Value, InventoryError> {
    manifest_for(root, "service/implexity", ".py", "service/implexity/**/*.py")
}


pub fn verify_implementation_manifest(root: &Path) -> Result<Value, InventoryError> {
    let expected = implementation_manifest(root)?;
    let path = root.join(MANIFEST_FILE);
    let differs = || InventoryError("Implementation source manifest differs from current bytes".into());
    if path.is_symlink() {
        return Err(differs());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| InventoryError(format!("{}: {e}", path.display())))?;
    let found: Value =
        implexity_core::json::parse_strict(&text).map_err(|e| InventoryError(e.to_string()))?;
    if found != expected {
        return Err(differs());
    }
    Ok(expected)
}


pub fn write_implementation_manifest(root: &Path) -> Result<Value, InventoryError> {
    let manifest = implementation_manifest(root)?;
    let mut text = dumps(&manifest, &DumpOptions::indented(2).sorted(true));
    text.push('\n');
    let dest = root.join(MANIFEST_FILE);
    std::fs::write(&dest, text).map_err(|e| InventoryError(format!("{}: {e}", dest.display())))?;
    Ok(manifest)
}

