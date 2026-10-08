// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde_json::Value;

use crate::json::{ParseOptions, parse_with};
use crate::py_repr::repr_str;
use crate::pyobj::list_repr;
use crate::sync::lock;

pub const MARKER: &str = "implexity_distribution.json";
pub const SCHEMA: &str = "implexity-distribution/1";
pub const CATALOGUE_KINDS: [&str; 5] =
    ["packages", "field_sources", "history_responses", "agent_workflows", "study_bundles"];
pub const MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DistributionError(pub String);

#[derive(Debug, Clone, Copy)]
pub struct EmbeddedDistribution {
    pub name: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistributionSource {
    Embedded(&'static str),
    Directory(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distribution {
    pub distribution_id: String,
    pub label: String,
    pub source: DistributionSource,
    pub catalogues: Vec<(String, String)>,
}

impl Distribution {
    #[must_use]
    pub fn resource(&self, kind: &str) -> Option<&str> {
        self.catalogues.iter().find(|(k, _)| k == kind).map(|(_, f)| f.as_str())
    }
}

fn is_snake(id: &str) -> bool {

    let mut parts = id.split('_');
    let Some(first) = parts.next() else { return false };
    let mut chars = first.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
        return false;
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return false;
    }
    parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
}

fn is_file_name(name: &str) -> bool {

    let Some(stem) = name.strip_suffix(".json") else { return false };
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && !stem.is_empty()
        && stem.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}


pub fn read_json_bytes(name: &str, raw: &[u8]) -> Result<Value, DistributionError> {
    if raw.len() > MAX_BYTES {
        return Err(DistributionError(format!("{name} exceeds the catalogue size budget")));
    }
    let text =
        std::str::from_utf8(raw).map_err(|_| DistributionError(format!("{name} is not valid UTF-8 JSON")))?;
    match parse_with(text, ParseOptions { reject_duplicate_keys: true }) {
        Ok(v) => Ok(v),
        Err(e) if e.message.starts_with("duplicate key ") => {
            Err(DistributionError(format!("{} in {name}", e.message)))
        }
        Err(_) => Err(DistributionError(format!("{name} is not valid UTF-8 JSON"))),
    }
}

fn parse_marker(
    dir_label: &str,
    doc: &Value,
    source: DistributionSource,
) -> Result<Distribution, DistributionError> {
    let invalid = || DistributionError(format!("{dir_label}/{MARKER}: invalid distribution declaration"));
    let Some(m) = doc.as_object() else { return Err(invalid()) };
    let keys: std::collections::BTreeSet<&str> = m.keys().map(String::as_str).collect();
    let expected: std::collections::BTreeSet<&str> =
        ["schema", "distribution_id", "label", "catalogues"].into_iter().collect();
    if keys != expected || m["schema"] != SCHEMA {
        return Err(invalid());
    }
    let ident = match m["distribution_id"].as_str() {
        Some(s) if is_snake(s) => s.to_string(),
        _ => {
            return Err(DistributionError(format!(
                "{dir_label}/{MARKER}: distribution_id must be snake_case"
            )));
        }
    };
    let label = match m["label"].as_str() {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Err(DistributionError(format!("{dir_label}/{MARKER}: label is required"))),
    };
    let Some(catalogues) = m["catalogues"].as_object().filter(|c| !c.is_empty()) else {
        return Err(DistributionError(format!("{dir_label}/{MARKER}: catalogues must be a nonempty object")));
    };
    let mut rows = Vec::new();
    for (kind, name) in catalogues {
        if !CATALOGUE_KINDS.contains(&kind.as_str()) {
            return Err(DistributionError(format!(
                "{dir_label}/{MARKER}: unknown catalogue kind {}; the kernel reads {}",
                repr_str(kind),
                list_repr(&CATALOGUE_KINDS)
            )));
        }
        match name.as_str() {
            Some(n) if is_file_name(n) => rows.push((kind.clone(), n.to_string())),
            _ => {
                return Err(DistributionError(format!(
                    "{dir_label}/{MARKER}: catalogue {} must name a JSON file in the distribution",
                    repr_str(kind)
                )));
            }
        }
    }
    Ok(Distribution { distribution_id: ident, label, source, catalogues: rows })
}

#[derive(Debug, Default)]
pub struct DistributionSet {
    embedded: Mutex<Vec<EmbeddedDistribution>>,
    registered: Mutex<Vec<PathBuf>>,
}

impl DistributionSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_embedded(&self, distribution: EmbeddedDistribution) {
        let mut e = lock(&self.embedded);
        if !e.iter().any(|d| d.name == distribution.name) {
            e.push(distribution);
            e.sort_by_key(|d| d.name);
        }
    }


    pub fn register_directory(&self, directory: &Path) -> Result<(), DistributionError> {
        if directory.as_os_str().is_empty() {
            return Err(DistributionError("distribution package name must be nonempty text".into()));
        }
        let mut r = lock(&self.registered);
        if !r.iter().any(|d| d == directory) {
            r.push(directory.to_path_buf());
        }
        Ok(())
    }

    fn directories(&self) -> Result<Vec<PathBuf>, DistributionError> {
        let mut names: Vec<PathBuf> = std::env::var("IMPLEXITY_DISTRIBUTIONS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        for r in lock(&self.registered).iter() {
            if !names.contains(r) {
                names.push(r.clone());
            }
        }
        let mut unique: Vec<PathBuf> = Vec::new();
        for name in names {
            let directory = std::fs::canonicalize(&name).map_err(|_| {
                DistributionError(format!(
                    "distribution package {} cannot be located",
                    repr_str(&name.display().to_string())
                ))
            })?;
            if !directory.is_dir() {
                return Err(DistributionError(format!(
                    "distribution {} is not a package directory",
                    repr_str(&name.display().to_string())
                )));
            }
            if !directory.join(MARKER).is_file() {
                return Err(DistributionError(format!(
                    "distribution package {} has no {MARKER}",
                    repr_str(&name.display().to_string())
                )));
            }
            if !unique.contains(&directory) {
                unique.push(directory);
            }
        }
        Ok(unique)
    }


    pub fn distributions(&self) -> Result<Vec<Distribution>, DistributionError> {
        let mut out: Vec<Distribution> = Vec::new();
        let embedded = lock(&self.embedded).clone();
        for e in embedded {
            let raw = e
                .files
                .iter()
                .find(|(n, _)| *n == MARKER)
                .map(|(_, c)| c.as_bytes())
                .ok_or_else(|| DistributionError(format!("cannot read {MARKER}")))?;
            let doc = read_json_bytes(MARKER, raw)?;
            out.push(parse_marker(e.name, &doc, DistributionSource::Embedded(e.name))?);
        }
        for dir in self.directories()? {
            let raw = std::fs::read(dir.join(MARKER))
                .map_err(|_| DistributionError(format!("cannot read {MARKER}")))?;
            let doc = read_json_bytes(MARKER, &raw)?;
            let label = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            out.push(parse_marker(&label, &doc, DistributionSource::Directory(dir))?);
        }
        let mut seen = std::collections::BTreeSet::new();
        for d in &out {
            if !seen.insert(d.distribution_id.clone()) {
                return Err(DistributionError(format!(
                    "two distributions declare id {}",
                    repr_str(&d.distribution_id)
                )));
            }
        }
        Ok(out)
    }

    fn read_resource(&self, dist: &Distribution, file: &str) -> Result<Value, DistributionError> {
        match &dist.source {
            DistributionSource::Embedded(name) => {
                let embedded = lock(&self.embedded).clone();
                let content = embedded
                    .iter()
                    .find(|e| e.name == *name)
                    .and_then(|e| e.files.iter().find(|(n, _)| *n == file))
                    .map(|(_, c)| *c)
                    .ok_or_else(|| DistributionError(format!("cannot read {file}")))?;
                read_json_bytes(file, content.as_bytes())
            }
            DistributionSource::Directory(dir) => {
                let raw = std::fs::read(dir.join(file))
                    .map_err(|_| DistributionError(format!("cannot read {file}")))?;
                read_json_bytes(file, &raw)
            }
        }
    }


    pub fn catalogue_documents(&self, kind: &str) -> Result<Vec<(String, Value)>, DistributionError> {
        if !CATALOGUE_KINDS.contains(&kind) {
            return Err(DistributionError(format!("unknown catalogue kind {}", repr_str(kind))));
        }
        let mut out = Vec::new();
        for dist in self.distributions()? {
            let Some(file) = dist.resource(kind) else { continue };
            out.push((dist.distribution_id.clone(), self.read_resource(&dist, file)?));
        }
        Ok(out)
    }
}

static GLOBAL: LazyLock<DistributionSet> = LazyLock::new(DistributionSet::new);

#[must_use]
pub fn global() -> &'static DistributionSet {
    &GLOBAL
}

