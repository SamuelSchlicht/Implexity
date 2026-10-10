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


use std::sync::LazyLock;

use serde_json::{Map, Value, json};

use implexity_core::json::{DumpOptions, dumps};
use implexity_core::pyobj::{py_str, truthy};

pub mod blocks;
pub mod embed;
pub mod records;
pub mod verify;

pub use blocks::{
    CONT_KEYS, FINGERPRINT_RECIPE, ObjectiveInputs, case_block, case_hashes, design_block,
    design_fingerprint, loss_trajectory, objective_block, steer_events,
};
pub use embed::{capability, embed, format_of, read_embedded, stamp, stl_header, vdb_metadata};
pub use records::{
    BodyRecordInputs, FieldExportInputs, OptimisationInputs, body_record, fidelity_body,
    fidelity_optimisation, field_export_record, hoist_warnings, optimisation_record,
};
pub use verify::{verify, verify_with_sidecar};

pub const SCHEMA: &str = "implexity-provenance/1";
pub const RECORD_VERSION: i64 = 1;
pub const MF_RELATION: &str = "http://schemas.implexity/provenance";
pub const MF_PART: &str = "/Metadata/implexity_record.json";
pub const PLY_CHUNK: usize = 512;
pub const STL_HEADER_BYTES: usize = 80;
pub const STL_PREFIX: &str = "implexity-provenance/1 ";

pub static STEP_CHUNK: LazyLock<usize> = LazyLock::new(|| {
    std::env::var("IMPLEXITY_STEP_COMMENT_CHUNK")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(4000)
});

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProvenanceError {
    #[error("{0}")]
    Embed(String),
    #[error("{0}")]
    Invalid(String),
}

impl ProvenanceError {
    #[must_use]
    pub fn py_repr(&self) -> String {
        match self {
            Self::Embed(m) => format!("EmbedError({})", implexity_core::py_repr::repr_str(m)),
            Self::Invalid(m) => format!("ValueError({})", implexity_core::py_repr::repr_str(m)),
        }
    }
}

pub(crate) fn invalid(msg: impl Into<String>) -> ProvenanceError {
    ProvenanceError::Invalid(msg.into())
}

pub(crate) fn embed_err(msg: impl Into<String>) -> ProvenanceError {
    ProvenanceError::Embed(msg.into())
}

pub type ProvResult<T> = Result<T, ProvenanceError>;

#[must_use]
pub fn jsonable_f64(v: f64) -> Value {
    if v.is_nan() {
        json!("NaN")
    } else if v.is_infinite() {
        json!(if v > 0.0 { "Infinity" } else { "-Infinity" })
    } else {
        json!(v)
    }
}

#[must_use]
pub fn pyfloat(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            let t = s.trim().replace('_', "");
            let lower = t.to_ascii_lowercase();
            let body = lower.trim_start_matches(['+', '-']);
            if matches!(body, "nan" | "inf" | "infinity") {
                let neg = lower.starts_with('-');
                return Some(match body {
                    "nan" => f64::NAN,
                    _ if neg => f64::NEG_INFINITY,
                    _ => f64::INFINITY,
                });
            }
            t.parse::<f64>().ok()
        }
        _ => None,
    }
}

pub(crate) fn need_float(value: &Value, what: &str) -> ProvResult<f64> {
    pyfloat(value)
        .ok_or_else(|| invalid(format!("{what}: float() argument must be a number, not {}", py_str(value))))
}

#[must_use]
pub fn pyint(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64().filter(|f| f.is_finite()).map(|f| {
                #[allow(clippy::cast_possible_truncation)]
                let t = f.trunc() as i64;
                t
            })
        }),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => s.trim().replace('_', "").parse::<i64>().ok(),
        _ => None,
    }
}

pub(crate) fn need_int(value: &Value, what: &str) -> ProvResult<i64> {
    pyint(value)
        .ok_or_else(|| invalid(format!("{what}: int() argument must be a number, not {}", py_str(value))))
}

#[must_use]
pub fn get<'a>(d: &'a Value, key: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    d.get(key).unwrap_or(&NULL)
}

#[must_use]
pub fn or<'a>(a: &'a Value, b: &'a Value) -> &'a Value {
    if truthy(a) { a } else { b }
}

#[must_use]
pub fn obj(x: &Value) -> Map<String, Value> {
    match x {
        Value::Object(m) if !m.is_empty() => m.clone(),
        _ => Map::new(),
    }
}

#[must_use]
pub fn list(x: &Value) -> Vec<Value> {
    match x {
        Value::Array(a) => a.clone(),
        Value::String(s) if !s.is_empty() => s.chars().map(|c| json!(c.to_string())).collect(),
        _ => Vec::new(),
    }
}

#[must_use]
pub fn format_e(v: f64, prec: usize) -> String {
    if !v.is_finite() {
        return if v.is_nan() {
            "nan".into()
        } else if v > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let s = format!("{v:.prec$e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let e: i32 = exp.parse().unwrap_or(0);
    format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
}

#[must_use]
pub fn canonical(record: &Value) -> String {
    implexity_core::json::canonical(record)
}

#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    crate::digest::sha256_hex(data)
}

#[must_use]
pub fn core(record: &Value) -> Value {
    let mut out = Map::new();
    if let Value::Object(m) = record {
        for (k, v) in m {
            if k != "record_id" && k != "stamped" {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

#[must_use]
pub fn compute_record_id(record: &Value) -> String {
    let full = sha256_hex(canonical(&core(record)).as_bytes());
    full[..16].to_string()
}

pub fn finalise(record: &mut Value) {
    let id = compute_record_id(record);
    if let Value::Object(m) = record {
        m.insert("record_id".into(), json!(id));
    }
}

#[must_use]
pub fn embedded_copy(record: &Value) -> Value {
    let mut out = core(record);
    let id = match get(record, "record_id") {
        v if truthy(v) => v.clone(),
        _ => json!(compute_record_id(record)),
    };
    if let Value::Object(m) = &mut out {
        m.insert("record_id".into(), id);
    }
    out
}

#[must_use]
pub fn sidecar_bytes(record: &Value) -> Vec<u8> {
    dumps(record, &DumpOptions::indented(1).sorted(true)).into_bytes()
}

fn platform_system() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Darwin",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        other => other,
    }
}

fn platform_machine() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "AMD64",
        ("windows", "aarch64") => "ARM64",
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    }
}

const PYTHON_LIBRARIES: [&str; 9] =
    ["numpy", "scipy", "jax", "jaxlib", "pymetis", "skimage", "trimesh", "lib3mf", "OCP"];


#[must_use]
pub fn environment(service_version: Option<&str>, backend: Option<&str>) -> Value {
    let mut libs = Map::new();
    for name in PYTHON_LIBRARIES {
        let mut row = json!({"present": false, "version": null, "detail": "not part of the Rust build"});
        if name == "OCP"
            && let Value::Object(m) = &mut row
        {
            m.insert("occt".into(), Value::Null);
        }
        libs.insert(name.into(), row);
    }
    libs.insert(
        "implexity-rs".into(),
        json!({"present": true, "version": implexity_core::RUST_VERSION, "detail": null}),
    );
    json!({
        "service": service_version,
        "backend": backend,
        "produced_at_utc": crate::digest::utc_timestamp(),
        "python": null,
        "runtime": "rust",
        "python_compatibility": implexity_core::PYTHON_COMPATIBILITY_VERSION,
        "platform": {"system": platform_system(), "machine": platform_machine()},
        "libraries": libs,
        "host_independent": true,
        "host_independent_note": "hostname, user, environment variables and absolute paths are deliberately NOT recorded here; paths that do appear elsewhere in this record are the service's own view of its inputs and are marked as such",
    })
}

