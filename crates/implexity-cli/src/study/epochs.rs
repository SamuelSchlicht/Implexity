// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;

use super::run::{history_length, is_generation, recorded_generations};
use super::{RResult, RunnerError, framework_root, read_json, rerr, sha256_file, write_json};
use crate::util::utc_iso;

const CONTINUATION_SCHEMA: &str = "implexity-optimization-continuation/1";

fn generation_of(from: &Path) -> RResult<PathBuf> {
    if is_generation(from) {
        return Ok(from.to_path_buf());
    }
    recorded_generations(from).into_iter().max_by_key(|d| history_length(d)).ok_or_else(|| {
        RunnerError::Runner(format!("No recorded provider-job generation under {}.", from.display()))
    })
}

pub(crate) fn reproduce_epoch(
    from: &str,
    epoch: Option<i64>,
    output: &str,
    framework: Option<&str>,
) -> RResult<u8> {
    let fw = framework_root(framework)?;
    let source = crate::util::expand_abs(from);
    let generation = std::fs::canonicalize(generation_of(&source)?)?;
    let length = history_length(&generation);
    if length == 0 {
        return rerr("The source generation has no recorded history.");
    }
    let epoch = match epoch {
        Some(e) if e < 0 || usize::try_from(e).unwrap_or(usize::MAX) >= length => {
            return rerr(format!("--epoch must lie in 0..{}.", length - 1));
        }
        Some(e) => usize::try_from(e).unwrap_or_default(),
        None => length - 1,
    };
    let output = crate::util::expand_abs(output);
    if output.exists() {
        return rerr(format!("--output {} exists; use a new directory.", output.display()));
    }
    let job = output.join("job");
    std::fs::create_dir_all(&job)?;
    let mut spec = read_json(&generation.join("spec.json"), 1 << 30)?;
    let Some(map) = spec.as_object_mut() else {
        return rerr("The recorded job specification is not an object.");
    };
    for name in implexity_jobs::hierarchical_job::input_file_names(map) {
        std::fs::copy(generation.join(&name), job.join(&name))?;
    }
    map.insert(
        "continuation".into(),
        json!({"schema": CONTINUATION_SCHEMA, "source_dir": generation.display().to_string(),
               "epoch": epoch, "mode": "reproduce"}),
    );
    write_json(&job.join("spec.json"), &spec, false)?;
    let state = implexity_jobs::epoch_state::read_epoch_state(&generation, epoch)
        .map_err(|e| RunnerError::Runner(format!("epoch state {epoch}: {}", e.message())))?;
    let started = utc_iso();
    let clock = std::time::Instant::now();
    let mut child = Command::new(&fw.service)
        .args(["worker", "provider-job", "--spec-file"])
        .arg(job.join("spec.json"))
        .arg("--job-dir")
        .arg(&job)
        .env("IMPLEXITY_PHYSICS_PACKAGES", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(output.join("worker_stderr.log"))?)
        .spawn()?;
    let mut lines = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line?;
            if line.starts_with("REPRODUCED ")
                || line.starts_with("ERROR ")
                || line.starts_with("CONTINUATION ")
            {
                lines.push(line);
            }
        }
    }
    let status = child.wait()?;
    let protocol = output.join("worker_protocol.jsonl");
    std::fs::write(&protocol, lines.join("\n") + "\n")?;
    let reproduction = read_json(&job.join("reproduction.json"), 1 << 30).ok();
    let error = lines.iter().find_map(|l| l.strip_prefix("ERROR ")).map(str::to_owned);
    let reproduced = reproduction.as_ref().is_some_and(|r| r["status"] == "reproduced");
    let report = json!({
        "schema": "implexity-study-epoch-reproduction/1",
        "utc": started,
        "wall_seconds": clock.elapsed().as_secs_f64(),
        "source": source.display().to_string(),
        "generation": generation.display().to_string(),
        "epoch": epoch,
        "epoch_state": state.as_ref().map(|s| s.manifest["archive"].clone()),
        "warm_start": reproduction.as_ref().map(|r| r["warm_start"].clone()),
        "executable": fw.service.display().to_string(),
        "executable_sha256": sha256_file(&fw.service)?,
        "recorded_executable_sha256": state.as_ref().map(|s| s.manifest["provenance"]["executable_sha256"].clone()),
        "exit_code": status.code(),
        "status": reproduction.as_ref().map_or_else(|| json!("failed"), |r| r["status"].clone()),
        "bitwise": reproduction.as_ref().map(|r| r["bitwise"].clone()),
        "relative_tolerance": reproduction.as_ref().map(|r| r["relative_tolerance"].clone()),
        "recorded": reproduction.as_ref().map(|r| r["recorded"].clone()),
        "replayed": reproduction.as_ref().map(|r| r["replayed"].clone()),
        "error": error,
    });
    write_json(&output.join("reproduction_report.json"), &report, false)?;
    println!(
        "epoch {epoch}: {} (bitwise {}, warm start {})",
        report["status"].as_str().unwrap_or("failed"),
        report["bitwise"],
        report["warm_start"]
    );
    Ok(if reproduced { 0 } else { 3 })
}

pub(crate) fn epoch_index(from: &str, output: Option<&str>) -> RResult<u8> {
    let source = crate::util::expand_abs(from);
    let generations =
        if is_generation(&source) { vec![source.clone()] } else { recorded_generations(&source) };
    if generations.is_empty() {
        return rerr(format!("No recorded provider-job generation under {}.", source.display()));
    }
    let mut indexed = Vec::new();
    for generation in &generations {
        let index = implexity_jobs::epoch_state::epoch_state_index(generation)
            .map_err(|e| RunnerError::Runner(format!("{}: {}", generation.display(), e.message())))?;
        indexed.push(index);
    }
    let report = json!({
        "schema": "implexity-study-epoch-index/1",
        "utc": utc_iso(),
        "source": source.display().to_string(),
        "generations": indexed,
    });
    let target = output.map_or_else(|| source.join("epochs_state_index.json"), crate::util::expand_abs);
    write_json(&target, &report, true)?;
    for index in &indexed {
        println!(
            "{}: {} epochs, design snapshots {}, full per-epoch states {}, warm starts {}",
            index["generation"].as_str().unwrap_or(""),
            index["epochs"],
            index["epochs_with_design_snapshot"],
            index["epochs_with_full_state"],
            index["epochs_with_warm_start"]
        );
    }
    println!("index: {}", target.display());
    Ok(0)
}

