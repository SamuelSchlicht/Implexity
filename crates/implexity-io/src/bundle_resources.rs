// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::path::{Path, PathBuf};

#[must_use]
pub fn installed_resource_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let root = exe.parent()?.join("_resources");
    root.is_dir().then_some(root)
}


pub fn implementation_source_root() -> Result<PathBuf, String> {
    Err("Source-attested acceleration requires an unpacked Python installation".into())
}

fn development_state() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().and_then(Path::parent).unwrap_or(manifest).join("state")
}

#[must_use]
pub fn state_directory() -> PathBuf {
    if let Some(explicit) = std::env::var_os("IMPLEXITY_CASE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(explicit);
    }
    state_directory_for(installed_resource_root().is_some(), home_directory())
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn state_directory_for(installed: bool, home: Option<PathBuf>) -> PathBuf {
    if installed && let Some(home) = home {
        return home.join(".implexity").join("state");
    }
    development_state()
}

pub const STAGE_MODULES: [(&str, &[&str]); 7] =
    [("36", &[]), ("37", &[]), ("38", &[]), ("39", &[]), ("40", &[]), ("41", &[]), ("42", &[])];

