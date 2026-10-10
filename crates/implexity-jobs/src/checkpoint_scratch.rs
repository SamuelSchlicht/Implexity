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

