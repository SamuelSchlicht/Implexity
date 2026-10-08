// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use implexity_render::mesh_scene::{StlFiles, parse_request, render_view};
use serde_json::{Value, json};

fn run(scene: &Path, out: &Path, only: &[String]) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(scene).map_err(|e| format!("cannot read {}: {e}", scene.display()))?;
    let raw: Value =
        serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", scene.display()))?;
    let req = parse_request(&raw).map_err(|e| e.to_string())?;
    for name in only {
        if !req.views.iter().any(|v| &v.name == name) {
            return Err(format!("the scene has no view {name}"));
        }
    }
    std::fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let base = scene.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut meshes = StlFiles::new(&base, &req);
    let mut reports = Vec::new();
    for v in req.views.iter().filter(|v| only.is_empty() || only.contains(&v.name)) {
        let r = render_view(&req, v, &mut meshes).map_err(|e| format!("view {}: {e}", v.name))?;
        std::fs::write(out.join(format!("{}.png", r.name)), r.png())
            .map_err(|e| format!("cannot write {}.png: {e}", r.name))?;
        let report = serde_json::to_string_pretty(&r.report).unwrap_or_default();
        std::fs::write(out.join(format!("{}.json", r.name)), report)
            .map_err(|e| format!("cannot write {}.json: {e}", r.name))?;
        eprintln!(
            "rendered {} ({}x{} px) in {:.1} s",
            r.name,
            r.width,
            r.height,
            r.report["elapsed_s"].as_f64().unwrap_or(0.0)
        );
        reports.push(r.report);
    }
    Ok(reports)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 || args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("usage: implexity-mesh-scene <scene.json> <out-dir> [view ...]");
        return ExitCode::from(2);
    }
    let (scene, out) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    match run(&scene, &out, &args[2..]) {
        Ok(reports) => {
            let doc = json!({"schema": "implexity-mesh-scene-report/1", "scene": scene.display().to_string(),
                             "views": reports});
            let written = std::fs::write(
                out.join("scene_report.json"),
                serde_json::to_string_pretty(&doc).unwrap_or_default(),
            );
            if let Err(e) = written {
                eprintln!("cannot write scene_report.json: {e}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("implexity-mesh-scene: {e}");
            ExitCode::FAILURE
        }
    }
}
