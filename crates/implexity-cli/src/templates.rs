// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::Path;

use serde_json::{Map, Value};

use implexity_core::contracts::ProviderCapabilities;

#[must_use]
pub(crate) fn merge(base: &Value, patch: &Value) -> Value {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let mut out: Map<String, Value> = b.clone();
            for (k, v) in p {
                let merged = merge(b.get(k).unwrap_or(&Value::Null), v);
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        _ => patch.clone(),
    }
}

fn editor_template(caps: &ProviderCapabilities) -> Option<Value> {
    match caps {
        ProviderCapabilities::Legacy(l) => l.editor.get("problem_template").cloned(),
        ProviderCapabilities::Descriptor(_) | ProviderCapabilities::Mapping(_) => None,
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn run(root: &Path) -> u8 {
    implexity_bundle::init();
    let mut failures: Vec<String> = Vec::new();
    if let Err(e) = implexity_server::startup::initialise_kernel() {
        failures.push(format!("kernel: RuntimeError: {e}"));
    }
    implexity_jobs::optimize::node::register_kind();
    let manager = implexity_core::packages::global();
    match manager.status() {
        Ok(status) => {
            for row in status["packages"].as_array().cloned().unwrap_or_default() {
                let name = row["id"].as_str().unwrap_or_default().to_owned();
                if let Err(e) = manager.load(&name) {
                    failures.push(format!("package {name}: {}: {}", e.python_class(), e.message()));
                }
            }
        }
        Err(e) => failures.push(format!("packages: {}: {}", e.python_class(), e.message())),
    }
    println!("loaded: {}", manager.selected().join(", "));

    let mut providers = 0usize;
    let mut templates = 0usize;
    for (name, provider) in implexity_core::registries::global().providers.snapshot().entries {
        let caps = match provider.capabilities() {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("provider {name} capabilities: {}: {}", e.python_class(), e.message()));
                continue;
            }
        };
        let Some(template) = editor_template(&caps) else { continue };
        if let Err(e) = provider.normalise_problem(&template) {
            failures.push(format!("provider {name} problem_template: {}: {}", e.python_class(), e.message()));
            continue;
        }
        providers += 1;
        let Some(ops) = implexity_optim::provider_ops::design_operations(provider.as_ref()) else { continue };
        let rows = match ops.workspace_declaration("study_templates", Some(&template)) {
            Some(Err(e)) => {
                failures.push(format!(
                    "provider {name} study_templates: {}: {}",
                    e.python_class(),
                    e.message()
                ));
                continue;
            }
            Some(Ok(Value::Array(rows))) => rows,
            None | Some(Ok(_)) => continue,
        };
        for row in rows {
            let patch = row
                .get("problem_patch")
                .filter(|p| !p.is_null())
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            match provider.normalise_problem(&merge(&template, &patch)) {
                Ok(_) => templates += 1,
                Err(e) => failures.push(format!(
                    "provider {name} study template {}: {}: {}",
                    implexity_core::pyobj::py_str(row.get("id").unwrap_or(&Value::Null)),
                    e.python_class(),
                    e.message()
                )),
            }
        }
    }

    let models = root.join("models");
    let mut stored: Vec<std::path::PathBuf> = std::fs::read_dir(&models)
        .map(|it| {
            it.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    stored.sort();
    for path in &stored {
        let result =
            implexity_core::json::read_file(path).map_err(|e| format!("ValueError: {e}")).and_then(|doc| {
                implexity_geometry::document::validate(&doc, Some(&models), None)
                    .map(|_| ())
                    .map_err(|e| format!("{}: {e}", crate::util::geo_class(&e)))
            });
        if let Err(e) = result {
            failures.push(format!("model {}: {e}", path.display()));
        }
    }
    let generated = match tempfile_dir().and_then(|dir| {
        let n = implexity_geometry::examples::write_all(&dir, None)
            .map(|rows| rows.len())
            .map_err(|e| format!("{}: {e}", crate::util::geo_class(&e)));
        let _ = std::fs::remove_dir_all(&dir);
        n
    }) {
        Ok(n) => n,
        Err(e) => {
            failures.push(format!("example models: {e}"));
            0
        }
    };
    println!(
        "provider templates: {providers}; study templates: {templates}; stored models: {}; generated examples: {generated}",
        stored.len()
    );
    for line in &failures {
        println!("FAIL {line}");
    }
    u8::from(!failures.is_empty())
}

fn tempfile_dir() -> Result<std::path::PathBuf, String> {
    let dir =
        std::env::temp_dir().join(format!("implexity-templates-{}", implexity_io::atomic::unique_token()));
    std::fs::create_dir(&dir).map_err(|e| format!("OSError: {e}"))?;
    Ok(dir)
}

pub(crate) fn main(argv: &[String]) -> u8 {
    match argv {
        [] => run(Path::new(".")),
        [root] if !root.starts_with('-') => run(Path::new(root)),
        _ => {
            eprintln!("usage: implexity validate-templates [ROOT]");
            2
        }
    }
}


