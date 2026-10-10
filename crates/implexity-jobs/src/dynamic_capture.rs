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




use std::path::{Path, PathBuf};

use implexity_core::contracts::Evaluation;
use implexity_io::fsguard::{create_dir_owner_only, stat_nofollow};
use implexity_runtime::dynamic_frames::capture::{self, CaptureRoot, DEFAULT_BUDGET_BYTES};
use implexity_runtime::dynamic_frames::catalogue::JOB_ROOT;
use implexity_runtime::dynamic_frames::import::import_evaluation;
use implexity_runtime::dynamic_frames::manifest::MIN_BYTE_LIMIT;

use crate::error::{JobError, JobResult};

pub const BUDGET_ENV: &str = "IMPLEXITY_DYNAMIC_CAPTURE_BUDGET_BYTES";

#[must_use]
pub fn capture_directory(job_dir: &Path) -> PathBuf {
    job_dir.join(JOB_ROOT)
}

fn io(what: &str, path: &Path, e: &std::io::Error) -> JobError {
    JobError::runtime(format!("dynamic capture {what} failed for {}: {e}", path.display()))
}



pub fn prepare_with(job_dir: &Path, lookup: impl Fn(&str) -> Option<String>) -> JobResult<CaptureRoot> {
    match stat_nofollow(job_dir) {
        Ok(st) if st.is_dir() && !st.is_symlink() => {}
        Ok(_) => {
            return Err(JobError::runtime(format!(
                "dynamic capture needs a job directory, not a link or file: {}",
                job_dir.display()
            )));
        }
        Err(e) => return Err(io("inspection", job_dir, &e)),
    }
    let budget_bytes = match lookup(BUDGET_ENV) {
        None => DEFAULT_BUDGET_BYTES,
        Some(s) => s.trim().parse::<u64>().ok().filter(|b| *b >= MIN_BYTE_LIMIT).ok_or_else(|| {
            JobError::runtime(format!("{BUDGET_ENV} must be an integer of at least {MIN_BYTE_LIMIT} bytes"))
        })?,
    };
    let dir = capture_directory(job_dir);
    match stat_nofollow(&dir) {
        Ok(st) if st.is_dir() && !st.is_symlink() => {}
        Ok(_) => {
            return Err(JobError::runtime(format!(
                "the job's dynamic capture directory is not a directory: {}",
                dir.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            create_dir_owner_only(&dir).map_err(|e| io("creation", &dir, &e))?;
        }
        Err(e) => return Err(io("inspection", &dir, &e)),
    }
    capture::mark_interrupted(&dir).map_err(|e| JobError::runtime(e.to_string()))?;
    Ok(CaptureRoot { dir, budget_bytes })
}



pub fn prepare(job_dir: &Path) -> JobResult<CaptureRoot> {
    prepare_with(job_dir, |k| std::env::var(k).ok())
}



pub fn import_result_frames(evaluation: &Evaluation, label: &str) -> JobResult<Option<PathBuf>> {
    match capture::current_root().map_err(|e| JobError::runtime(e.to_string()))? {
        Some(root) => import_result_frames_into(&root, evaluation, label),
        None => Ok(None),
    }
}



pub fn import_result_frames_into(
    root: &CaptureRoot,
    evaluation: &Evaluation,
    label: &str,
) -> JobResult<Option<PathBuf>> {
    if !evaluation.fields.keys().any(|k| k.starts_with("frame_") || k.starts_with("cycle_")) {
        return Ok(None);
    }
    import_evaluation(root, &evaluation.provider, &evaluation.fields, &evaluation.diagnostics, label)
        .map(|s| s.map(|s| s.dir))
        .map_err(|e| JobError::runtime(e.to_string()))
}

