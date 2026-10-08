// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::path::{Path, PathBuf};

use implexity_io::fsguard::{create_dir_owner_only, stat_nofollow};
use implexity_solve::state_store::{DISK_BUDGET_ENV, DISK_ROOT_ENV, RAM_BUDGET_ENV, StoreBudget};

use crate::error::{JobError, JobResult};

pub const SCRATCH_DIRECTORY: &str = "scratch";
pub const CHECKPOINT_DIRECTORY: &str = "checkpoints";

#[must_use]
pub fn checkpoint_directory(job_dir: &Path) -> PathBuf {
    job_dir.join(SCRATCH_DIRECTORY).join(CHECKPOINT_DIRECTORY)
}

fn io(what: &str, path: &Path, e: &std::io::Error) -> JobError {
    JobError::runtime(format!("checkpoint scratch {what} failed for {}: {e}", path.display()))
}

fn remove_nofollow(path: &Path) -> JobResult<bool> {
    match stat_nofollow(path) {
        Ok(st) if st.is_dir() && !st.is_symlink() => {
            std::fs::remove_dir_all(path).map_err(|e| io("removal", path, &e))?;
            Ok(true)
        }
        Ok(_) => {
            std::fs::remove_file(path).map_err(|e| io("removal", path, &e))?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io("inspection", path, &e)),
    }
}



pub fn remove_checkpoint_scratch(job_dir: &Path) -> JobResult<bool> {
    let scratch = job_dir.join(SCRATCH_DIRECTORY);
    match stat_nofollow(&scratch) {
        Ok(st) if st.is_dir() && !st.is_symlink() => {}
        Ok(_) => {
            remove_nofollow(&scratch)?;
            return Ok(false);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(io("inspection", &scratch, &e)),
    }
    let removed = remove_nofollow(&scratch.join(CHECKPOINT_DIRECTORY))?;
    let empty = std::fs::read_dir(&scratch).map_err(|e| io("listing", &scratch, &e))?.next().is_none();
    if empty {
        std::fs::remove_dir(&scratch).map_err(|e| io("removal", &scratch, &e))?;
    }
    Ok(removed)
}

#[derive(Debug)]
pub struct CheckpointScratch {
    job_dir: PathBuf,
    directory: PathBuf,
    budget: StoreBudget,
    removed: bool,
}

impl CheckpointScratch {


    pub fn prepare(job_dir: &Path) -> JobResult<Self> {
        Self::prepare_with(job_dir, |name| std::env::var(name).ok())
    }



    pub fn prepare_with(job_dir: &Path, lookup: impl Fn(&str) -> Option<String>) -> JobResult<Self> {
        match stat_nofollow(job_dir) {
            Ok(st) if st.is_dir() && !st.is_symlink() => {}
            Ok(_) => {
                return Err(JobError::runtime(format!(
                    "checkpoint scratch needs a job directory, not a link or file: {}",
                    job_dir.display()
                )));
            }
            Err(e) => return Err(io("inspection", job_dir, &e)),
        }
        let directory = checkpoint_directory(job_dir);
        let root = directory.display().to_string();
        let budget = StoreBudget::from_lookup(|name| match name {
            n if n == DISK_ROOT_ENV => Some(root.clone()),
            n if n == RAM_BUDGET_ENV || n == DISK_BUDGET_ENV => lookup(n),
            _ => None,
        })
        .map_err(JobError::from)?;
        remove_checkpoint_scratch(job_dir)?;
        let scratch = job_dir.join(SCRATCH_DIRECTORY);

        match create_dir_owner_only(&scratch) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io("creation", &scratch, &e)),
        }
        create_dir_owner_only(&directory).map_err(|e| io("creation", &directory, &e))?;
        Ok(Self { job_dir: job_dir.to_path_buf(), directory, budget, removed: false })
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub const fn budget(&self) -> &StoreBudget {
        &self.budget
    }

    #[must_use]
    pub fn environment(&self) -> Vec<(String, String)> {
        self.budget.environment()
    }



    pub fn remove(mut self) -> JobResult<bool> {
        self.removed = true;
        remove_checkpoint_scratch(&self.job_dir)
    }
}

impl Drop for CheckpointScratch {
    fn drop(&mut self) {
        if !self.removed {
            let _ = remove_checkpoint_scratch(&self.job_dir);
        }
    }
}

