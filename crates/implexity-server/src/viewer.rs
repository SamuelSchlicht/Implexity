// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

