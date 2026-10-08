// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

