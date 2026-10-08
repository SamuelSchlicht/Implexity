// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use implexity_core::distributions::{DistributionSet, global};

pub const LAYOUT_SCHEMA: &str = "implexity-study-bundle-layouts/1";
const LAYOUT_FIELDS: [&str; 7] =
    ["design_candidates", "design_glob", "label", "lattice_dir", "layout_id", "markers", "required_files"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LocateError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub layout_id: String,
    pub label: String,
    pub markers: Vec<String>,
    pub required_files: Vec<String>,
    pub design_candidates: Vec<String>,
    pub design_glob: String,
    pub lattice_dir: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum Environ<'a> {
    #[default]
    Process,
    Map(&'a BTreeMap<String, String>),
}

impl Environ<'_> {
    fn get(self, key: &str) -> Option<String> {
        match self {
            Self::Process => std::env::var(key).ok(),
            Self::Map(m) => m.get(key).cloned(),
        }
        .filter(|v| !v.is_empty())
    }
}

fn relative(path: &Value) -> bool {
    path.as_str()
        .is_some_and(|p| !p.is_empty() && !Path::new(p).is_absolute() && !p.split('/').any(|s| s == ".."))
}

fn native(rel: &str) -> PathBuf {
    rel.split('/').collect()
}

fn native_str(rel: &str) -> String {
    native(rel).to_string_lossy().into_owned()
}


pub fn layouts_in(set: &DistributionSet) -> Result<Vec<Layout>, LocateError> {
    let mut out = Vec::new();
    let docs = set.catalogue_documents("study_bundles").map_err(|e| LocateError(e.0))?;
    for (distribution_id, document) in docs {
        let valid = document.as_object().is_some_and(|o| {
            o.len() == 2
                && o.get("schema").and_then(Value::as_str) == Some(LAYOUT_SCHEMA)
                && o.get("layouts").is_some_and(Value::is_array)
        });
        if !valid {
            return Err(LocateError(format!("{distribution_id}: invalid study-bundle layout catalogue")));
        }
        for row in document["layouts"].as_array().into_iter().flatten() {
            let Some(o) = row.as_object().filter(|o| {
                o.len() == LAYOUT_FIELDS.len() && LAYOUT_FIELDS.iter().all(|k| o.contains_key(*k))
            }) else {
                return Err(LocateError(format!(
                    "{distribution_id}: study-bundle layout has unknown or missing fields"
                )));
            };
            let lists = ["markers", "required_files", "design_candidates"];
            let lists_ok = lists.iter().all(|k| o[*k].as_array().is_some_and(|a| a.iter().all(relative)));
            if !lists_ok
                || o["markers"].as_array().is_none_or(Vec::is_empty)
                || !relative(&o["design_glob"])
                || !relative(&o["lattice_dir"])
            {
                return Err(LocateError(format!(
                    "{distribution_id}: study-bundle layout paths must be relative"
                )));
            }
            let list = |k: &str| -> Vec<String> {
                o[k].as_array().into_iter().flatten().filter_map(Value::as_str).map(native_str).collect()
            };
            let text = |k: &str| -> String {
                match &o[k] {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                }
            };
            out.push(Layout {
                layout_id: text("layout_id"),
                label: text("label"),
                markers: list("markers"),
                required_files: list("required_files"),
                design_candidates: list("design_candidates"),
                design_glob: text("design_glob"),
                lattice_dir: text("lattice_dir"),
            });
        }
    }
    Ok(out)
}


pub fn layouts() -> Result<Vec<Layout>, LocateError> {
    layouts_in(global())
}

#[must_use]
pub fn abspath(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Locator {
    layouts: Vec<Layout>,
}

impl Locator {

    pub fn new() -> Result<Self, LocateError> {
        Ok(Self { layouts: layouts()? })
    }

    #[must_use]
    pub fn with_layouts(layouts: Vec<Layout>) -> Self {
        Self { layouts }
    }

    #[must_use]
    pub fn layouts(&self) -> &[Layout] {
        &self.layouts
    }

    fn layout_of(&self, path: &Path) -> Option<&Layout> {
        if path.as_os_str().is_empty() || !path.is_dir() {
            return None;
        }
        self.layouts.iter().find(|l| l.markers.iter().all(|m| path.join(m).is_dir()))
    }

    #[must_use]
    pub fn is_bundle(&self, path: &Path) -> bool {
        self.layout_of(path).is_some()
    }

    #[must_use]
    pub fn required_files(&self, path: Option<&Path>) -> Vec<String> {
        if let Some(layout) = path.and_then(|p| self.layout_of(p)) {
            return layout.required_files.clone();
        }
        self.layouts.iter().flat_map(|l| l.required_files.iter().cloned()).collect()
    }

    #[must_use]
    pub fn bundle_gaps(&self, path: &Path) -> Vec<String> {
        if path.as_os_str().is_empty() || !path.is_dir() {
            return vec![format!("not a directory: {}", path.display())];
        }
        let Some(layout) = self.layout_of(path) else {
            let Some(first) = self.layouts.first() else {
                return vec!["no installed distribution declares a study-bundle layout".into()];
            };
            return first
                .markers
                .iter()
                .filter(|m| !path.join(m).is_dir())
                .map(|m| format!("{m}/"))
                .collect();
        };
        let mut gaps: Vec<String> =
            layout.required_files.iter().filter(|r| !path.join(r).is_file()).cloned().collect();
        if design_in(path, layout).is_none() {
            gaps.push(layout.design_glob.clone());
        }
        gaps
    }

    #[must_use]
    pub fn probe_candidates(&self, start: &Path) -> Vec<(PathBuf, Vec<String>)> {
        let mut out = Vec::new();
        let mut d = abspath(start);
        loop {
            if self.is_bundle(&d) {
                let gaps = self.bundle_gaps(&d);
                out.push((d.clone(), gaps));
            }
            match d.parent() {
                Some(p) if p != d => d = p.to_path_buf(),
                _ => return out,
            }
        }
    }

    #[must_use]
    pub fn probe_upward(&self, start: &Path) -> Option<PathBuf> {
        let cands = self.probe_candidates(start);
        if let Some((p, _)) = cands.iter().find(|(_, g)| g.is_empty()) {
            return Some(p.clone());
        }
        cands.into_iter().next().map(|(p, _)| p)
    }

    #[must_use]
    pub fn find_bundle(
        &self,
        explicit: Option<&Path>,
        environ: Environ<'_>,
        start: Option<&Path>,
    ) -> Option<PathBuf> {
        if let Some(e) = explicit.filter(|e| !e.as_os_str().is_empty()) {
            return Some(abspath(e));
        }
        if let Some(v) = environ.get("IMPLEXITY_BUNDLE") {
            return Some(abspath(Path::new(&v)));
        }
        let start = start.map_or_else(default_start, Path::to_path_buf);
        self.probe_upward(&start)
    }

    #[must_use]
    pub fn resolve_lattice_full(&self, bundle: Option<&Path>, environ: Environ<'_>) -> Option<PathBuf> {
        if let Some(v) = environ.get("IMPLEXITY_LATTICE_FULL") {
            return Some(abspath(Path::new(&v)));
        }
        let bundle = bundle.map(Path::to_path_buf).or_else(|| self.find_bundle(None, environ, None))?;
        let layout = self.layout_of(&bundle)?;
        let cand = bundle.join(&layout.lattice_dir);
        cand.is_dir().then_some(cand)
    }

    #[must_use]
    pub fn resolve_design(
        &self,
        bundle: Option<&Path>,
        explicit: Option<&Path>,
        environ: Environ<'_>,
    ) -> Option<PathBuf> {
        if let Some(e) = explicit.filter(|e| !e.as_os_str().is_empty()) {
            return Some(abspath(e));
        }
        if let Some(v) = environ.get("IMPLEXITY_DESIGN") {
            return Some(abspath(Path::new(&v)));
        }
        let bundle = bundle.map(Path::to_path_buf).or_else(|| self.find_bundle(None, environ, None))?;
        let layout = self.layout_of(&bundle)?;
        design_in(&bundle, layout)
    }

    #[must_use]
    pub fn describe(
        &self,
        bundle: Option<&Path>,
        design: Option<&Path>,
        lattice: Option<&Path>,
        start: Option<&Path>,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let gaps = bundle.map(|b| self.bundle_gaps(b)).unwrap_or_default();
        let warning = if gaps.is_empty() {
            String::new()
        } else {
            let mut shown: Vec<String> = gaps.iter().take(4).cloned().collect();
            if gaps.len() > 4 {
                shown.push("...".into());
            }
            format!("  [WARNING: incomplete -- missing {}]", shown.join(", "))
        };
        lines.push(format!(
            "bundle:        {}{warning}",
            bundle.map_or_else(|| "(none found)".to_string(), |b| b.display().to_string())
        ));
        lines.push(format!(
            "design:        {}",
            design
                .filter(|d| d.is_file())
                .map_or_else(|| "(none found -> synthetic backend)".to_string(), |d| d.display().to_string())
        ));
        lines.push(format!(
            "lattice code:  {}",
            lattice
                .map_or_else(|| "(none found -> synthetic backend)".to_string(), |l| l.display().to_string())
        ));
        let start = start.map_or_else(default_start, Path::to_path_buf);
        for (path, pgaps) in self.probe_candidates(&start) {
            if Some(path.as_path()) != bundle {
                let state = if pgaps.is_empty() {
                    "complete".to_string()
                } else {
                    let mut shown: Vec<String> = pgaps.iter().take(3).cloned().collect();
                    if pgaps.len() > 3 {
                        shown.push("...".into());
                    }
                    format!("missing {}", shown.join(", "))
                };
                lines.push(format!("               (also seen: {} -- {state})", path.display()));
            }
        }
        lines
    }
}

fn default_start() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| abspath(Path::new(".")))
}

fn design_in(path: &Path, layout: &Layout) -> Option<PathBuf> {
    for rel in &layout.design_candidates {
        let cand = path.join(rel);
        if cand.is_file() {
            return Some(cand);
        }
    }
    let mut hits = glob(path, &layout.design_glob);
    hits.sort();
    hits.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    hits.pop()
}

#[must_use]
pub fn glob(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut current = vec![root.to_path_buf()];
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        let mut next = Vec::new();
        for dir in &current {
            if !seg.contains(['*', '?', '[']) {
                let p = dir.join(seg);
                if if last { p.exists() || p.is_symlink() } else { p.is_dir() } {
                    next.push(p);
                }
                continue;
            }
            let Ok(rd) = std::fs::read_dir(dir) else { continue };
            let mut names: Vec<String> =
                rd.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            for name in names {
                if name.starts_with('.') && !seg.starts_with('.') {
                    continue;
                }
                if fnmatch(&name, seg) {
                    let p = dir.join(&name);
                    if last || p.is_dir() {
                        next.push(p);
                    }
                }
            }
        }
        current = next;
    }
    if segments.is_empty() { Vec::new() } else { current }
}

#[must_use]
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    let n: Vec<char> = name.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    match_from(&n, &p)
}

fn match_from(n: &[char], p: &[char]) -> bool {
    let Some((&first, rest)) = p.split_first() else { return n.is_empty() };
    match first {
        '*' => (0..=n.len()).any(|k| match_from(&n[k..], rest)),
        '?' => !n.is_empty() && match_from(&n[1..], rest),
        '[' => {

            let mut j = 0;
            if rest.first() == Some(&'!') {
                j += 1;
            }
            if rest.get(j) == Some(&']') {
                j += 1;
            }
            while j < rest.len() && rest[j] != ']' {
                j += 1;
            }
            if j >= rest.len() {
                return n.first() == Some(&'[') && match_from(&n[1..], rest);
            }
            let Some(&c) = n.first() else { return false };
            let (negate, class) =
                if rest.first() == Some(&'!') { (true, &rest[1..j]) } else { (false, &rest[..j]) };
            let mut hit = false;
            let mut k = 0;
            while k < class.len() {
                if k + 2 < class.len() && class[k + 1] == '-' {
                    if class[k] <= c && c <= class[k + 2] {
                        hit = true;
                    }
                    k += 3;
                } else {
                    if class[k] == c {
                        hit = true;
                    }
                    k += 1;
                }
            }
            hit != negate && match_from(&n[1..], &rest[j + 1..])
        }
        lit => n.first() == Some(&lit) && match_from(&n[1..], rest),
    }
}

