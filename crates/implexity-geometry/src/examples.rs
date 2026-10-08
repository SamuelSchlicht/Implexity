// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
        needs: "nothing (jax to evaluate)".into(),
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

