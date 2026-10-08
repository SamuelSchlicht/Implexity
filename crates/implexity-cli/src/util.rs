// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;


#[must_use]
pub(crate) fn textwrap(text: &str, width: usize) -> Vec<String> {
    let text: String = text.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).collect();
    let mut chunks: Vec<&str> = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    for i in 1..=bytes.len() {
        if i == bytes.len() || (bytes[i] == b' ') != (bytes[start] == b' ') {
            chunks.push(&text[start..i]);
            start = i;
        }
    }
    let mut lines = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut len = 0usize;
    let mut k = 0;
    while k < chunks.len() {
        let c = chunks[k];
        if cur.is_empty() && c.starts_with(' ') {
            k += 1;
            continue;
        }
        let n = c.chars().count();
        if len + n <= width.max(1) {
            cur.push(c);
            len += n;
            k += 1;
            continue;
        }
        if cur.is_empty() {

            cur.push(c);
            k += 1;
        }
        while cur.last().is_some_and(|c| c.starts_with(' ')) {
            cur.pop();
        }
        lines.push(cur.concat());
        cur.clear();
        len = 0;
    }
    while cur.last().is_some_and(|c| c.starts_with(' ')) {
        cur.pop();
    }
    if !cur.is_empty() {
        lines.push(cur.concat());
    }
    lines
}

#[must_use]
pub(crate) fn wrapped(prefix: &str, text: &str, width: usize, min_body: usize) -> Vec<String> {
    let plen = prefix.chars().count();
    let pad = " ".repeat(plen);
    let mut lines = textwrap(text, width.saturating_sub(plen).max(min_body));
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| format!("{}{l}", if i == 0 { prefix } else { pad.as_str() }))
        .collect()
}

pub(crate) fn wrap_out(prefix: &str, text: &str, width: usize, min_body: usize) {
    for line in wrapped(prefix, text, width, min_body) {
        println!("{line}");
    }
}

pub(crate) fn wrap_err(prefix: &str, text: &str, width: usize, min_body: usize) {
    let mut err = std::io::stderr().lock();
    for line in wrapped(prefix, text, width, min_body) {
        let _ = writeln!(err, "{line}");
    }
}

#[must_use]
pub(crate) fn clip(text: &str, n: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    trunc(&text, n)
}

#[must_use]
pub(crate) fn trunc(text: &str, n: usize) -> String {
    if text.chars().count() <= n {
        return text.to_owned();
    }
    let mut s: String = text.chars().take(n.saturating_sub(1)).collect();
    s.push('…');
    s
}

#[must_use]
pub(crate) fn oneline(text: &str, n: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= n {
        return text;
    }
    let mut s: String = text.chars().take(n.saturating_sub(3)).collect();
    s.push_str("...");
    s
}

#[must_use]
pub(crate) fn dumps_indent(value: &Value) -> String {
    implexity_core::json::dumps(value, &implexity_core::json::DumpOptions::indented(2))
}

#[must_use]
pub(crate) fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .map(str::to_ascii_lowercase)
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("{name}{ext}"));
            if is_executable(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[must_use]
pub(crate) fn expand_abs(p: &str) -> PathBuf {
    let expanded = if let Some(rest) = p.strip_prefix("~/").or_else(|| (p == "~").then_some("")) {
        home().map_or_else(|| PathBuf::from(p), |h| h.join(rest))
    } else {
        PathBuf::from(p)
    };
    implexity_io::locate::abspath(&expanded)
}

#[must_use]
pub(crate) fn home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[must_use]
pub(crate) fn getenv(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[must_use]
pub(crate) fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf))
}

#[must_use]
pub(crate) fn sibling_executable(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let dir = exe_dir()?;
    [dir.join(&file), dir.parent().map(|p| p.join(&file)).unwrap_or_default()]
        .into_iter()
        .find(|p| p.is_file())
}

#[must_use]
pub(crate) fn utc_iso() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = i64::try_from(now.as_secs()).unwrap_or(0);
    let base = implexity_io::digest::format_utc(secs);
    format!("{}.{:06}+00:00", base.trim_end_matches('Z'), now.subsec_micros())
}

#[must_use]
pub(crate) fn geo_class(e: &implexity_geometry::GeometryError) -> &'static str {
    use implexity_geometry::GeometryError as G;
    match e {
        G::Model(_) => "ModelError",
        G::ModelDoc(_) => "ModelDocError",
        G::Expr(_) => "ExprError",
        G::BoundViolation(_) => "BoundViolation",
        G::Transpile { .. } => "TranspileError",
        G::Value(_) => "ValueError",
        G::Key(_) => "KeyError",
        G::Cancelled => "Cancelled",
        G::Io(_) => "OSError",
    }
}

