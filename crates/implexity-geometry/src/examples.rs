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

use implexity_core::contributions::ContributionRegistry;

use crate::error::{GResult, GeometryError};

pub const MODELS_DIRNAME: &str = "models";
pub const TREE_MARKERS: [&str; 2] = ["clients", "viewer"];

#[derive(Clone, Debug, PartialEq)]
pub struct Example {
    pub key: String,
    pub filename: String,
    pub title: String,
    pub teaches: String,
    pub needs: String,
    pub commands: Vec<String>,
    pub text: String,
}

impl Example {

    pub fn document(&self) -> GResult<Value> {
        serde_json::from_str(&self.text).map_err(|e| GeometryError::Value(e.to_string()))
    }

    #[must_use]
    pub fn describe(&self) -> Value {
        json!({"key": self.key, "file": self.filename, "title": self.title, "teaches": self.teaches, "needs": self.needs,
            "commands": self.commands})
    }
}

fn ex(key: &str, filename: &str, title: &str, teaches: &str, commands: &[&str], text: &str) -> Example {
    Example {
        key: key.into(),
        filename: filename.into(),
        title: title.into(),
        teaches: teaches.into(),
        needs: "None".into(),
        commands: commands.iter().map(|s| (*s).to_string()).collect(),
        text: text.into(),
    }
}

#[must_use]
pub fn kernel_examples() -> Vec<Example> {
    vec![
        ex(
            "bracket",
            "01_bracket.json",
            "a shelled bracket: primitives, a boolean, a fillet, a shell",
            "the whole vocabulary in seven nodes -- named parameters, one derived expression, and the EXACT -> BOUND field-class chain",
            &[
                "implexity model show models/01_bracket.json",
                "implexity model eval models/01_bracket.json --node solid --at 10,0,0",
                "implexity model export models/01_bracket.json -o /tmp/bracket.stl",
            ],
            include_str!("../data/models/01_bracket.json"),
        ),
        ex(
            "yoke",
            "02_shared_yoke.json",
            "a clamp yoke: ONE bolt-hole node with FOUR parents",
            "the DAG -- why the document is a node table and not a nested tree, and why sharing is by identity and never by content",
            &[
                "implexity model show models/02_shared_yoke.json",
                "implexity model set models/02_shared_yoke.json bore_d=6.5 --dry-run",
            ],
            include_str!("../data/models/02_shared_yoke.json"),
        ),
        ex(
            "lattice",
            "03_lattice_block.json",
            "a cooled block: solid skin, gyroid sheet lattice core",
            "a lattice INSIDE a part -- confined by intersect, BOUND throughout, so it can still be offset, traced and meshed",
            &[
                "implexity model show models/03_lattice_block.json",
                "implexity model eval models/03_lattice_block.json --node cells --grid 24",
                "implexity model export models/03_lattice_block.json -o /tmp/block.stl --spacing 0.35",
            ],
            include_str!("../data/models/03_lattice_block.json"),
        ),
    ]
}

#[must_use]
pub fn examples() -> Vec<Example> {
    examples_in(&implexity_core::registries::global().contributions)
}

#[must_use]
pub fn examples_in(reg: &ContributionRegistry) -> Vec<Example> {
    let mut v = kernel_examples();
    let contributed = reg.entries("model_examples").unwrap_or_default();
    v.extend(contributed.iter().filter_map(|(_, value)| value.downcast::<Example>()).map(|e| (*e).clone()));
    v
}

#[must_use]
pub fn by_key(key_or_file: &str) -> Option<Example> {
    examples().into_iter().find(|e| e.key == key_or_file || e.filename == key_or_file)
}

#[must_use]
pub fn models_dir(tree_root: Option<&Path>) -> Option<PathBuf> {
    if let Ok(env) = std::env::var("IMPLEXITY_MODELS") {
        let p = PathBuf::from(&env);
        if !env.is_empty() && p.is_dir() {
            return std::path::absolute(&p).ok();
        }
    }
    if let Some(root) = tree_root {
        let d = root.join(MODELS_DIRNAME);
        if d.is_dir() {
            return std::path::absolute(&d).ok();
        }
    }
    let mut here = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let mut fallback = None;
    for _ in 0..6 {
        let Some(parent) = here.parent().map(Path::to_path_buf) else { break };
        here = parent;
        let d = here.join(MODELS_DIRNAME);
        if !d.is_dir() {
            continue;
        }
        if TREE_MARKERS.iter().all(|m| here.join(m).is_dir()) {
            return Some(d);
        }
        fallback = fallback.or(Some(d));
    }
    fallback
}

#[must_use]
pub fn path_of(key_or_file: &str, tree_root: Option<&Path>) -> Option<PathBuf> {
    let e = by_key(key_or_file)?;
    Some(models_dir(tree_root)?.join(e.filename))
}


pub fn write_all(directory: &Path, only: Option<&[String]>) -> GResult<Vec<Value>> {
    std::fs::create_dir_all(directory).map_err(|e| GeometryError::Io(e.to_string()))?;
    let mut rows = Vec::new();
    for e in examples() {
        if let Some(o) = only
            && !o.iter().any(|k| *k == e.key || *k == e.filename)
        {
            continue;
        }
        let path = directory.join(&e.filename);
        let (norm, warnings) = crate::document::write(
            &path,
            &e.document()?,
            &std::collections::BTreeMap::new(),
            Some(directory),
        )?;
        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        let count = |k: &str| norm.get(k).and_then(Value::as_object).map_or(0, serde_json::Map::len);
        rows.push(json!({"key": e.key, "file": e.filename, "path": path.to_string_lossy(), "bytes": bytes,
            "nodes": count("nodes"), "parameters": count("parameters"), "arrays": count("arrays"), "warnings": warnings}));
    }
    Ok(rows)
}

