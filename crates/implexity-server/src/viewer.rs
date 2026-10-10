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

use crate::http::Reply;
use crate::viewer_assets::FILES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerSource {
    Directory(PathBuf),
    Embedded,
    Absent,
}

impl ViewerSource {
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var_os("IMPLEXITY_VIEWER_DIR") {
            Some(d) if !d.is_empty() && Path::new(&d).is_dir() => Self::Directory(PathBuf::from(d)),
            _ => Self::Embedded,
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Directory(d) => d.display().to_string(),
            Self::Embedded => "embedded".to_owned(),
            Self::Absent => "absent".to_owned(),
        }
    }

    #[must_use]
    pub fn is_present(&self) -> bool {
        !matches!(self, Self::Absent)
    }

    #[must_use]
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        match self {
            Self::Absent => None,
            Self::Embedded => {
                let key = normalise(name)?;
                FILES.binary_search_by(|(n, _)| (*n).cmp(key.as_str())).ok().map(|i| FILES[i].1.to_vec())
            }
            Self::Directory(base) => {
                let base = std::fs::canonicalize(base).ok()?;
                let full = std::fs::canonicalize(base.join(name)).ok()?;
                if !full.starts_with(&base) || !full.is_file() {
                    return None;
                }
                std::fs::read(full).ok()
            }
        }
    }
}

fn normalise(name: &str) -> Option<String> {
    if name.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in name.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn content_type(name: &str) -> &'static str {
    if name.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if name.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if name.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/octet-stream; charset=utf-8"
    }
}

#[must_use]
pub fn serve(source: &ViewerSource, name: &str) -> Reply {
    if !source.is_present() {
        return Reply::err(404, "no viewer directory configured", Value::Null);
    }
    match source.read(name) {
        Some(bytes) => {
            let resolved = normalise(name).unwrap_or_else(|| name.to_owned());
            Reply::send(200, bytes, content_type(&resolved), Vec::new())
        }
        None => Reply::err(404, "no such viewer file", json!(name)),
    }
}

#[must_use]
pub fn embedded_files() -> &'static [(&'static str, &'static [u8])] {
    FILES
}

