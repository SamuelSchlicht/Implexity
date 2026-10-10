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

