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




use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub const LEASE_FILENAME: &str = ".implexity-heavy-operation.lock";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HeavyOperationLeaseError(pub String);

fn fail(msg: &str) -> HeavyOperationLeaseError {
    HeavyOperationLeaseError(msg.to_string())
}

struct State {
    held: bool,
    file: Option<File>,
}

pub struct HeavyOperationLease {
    path: PathBuf,
    state: Mutex<State>,
    freed: Condvar,
}

impl std::fmt::Debug for HeavyOperationLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeavyOperationLease")
            .field("path", &self.path)
            .field("locked", &self.locked())
            .finish_non_exhaustive()
    }
}

impl HeavyOperationLease {

    pub fn new(state_directory: &Path) -> Result<Self, HeavyOperationLeaseError> {
        let state = if state_directory.is_absolute() {
            state_directory.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|_| fail("heavy-operation state directory is unavailable"))?
                .join(state_directory)
        };
        let meta = std::fs::symlink_metadata(&state)
            .map_err(|_| fail("heavy-operation state directory is unavailable"))?;
        let unsafe_dir = || fail("heavy-operation state directory ownership, mode, or type is unsafe");
        if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
            return Err(unsafe_dir());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if crate::atomic::current_uid().is_some_and(|u| u != meta.uid())
                || meta.permissions().mode() & 0o022 != 0
            {
                return Err(unsafe_dir());
            }
        }
        Ok(Self {
            path: state.join(LEASE_FILENAME),
            state: Mutex::new(State { held: false, file: None }),
            freed: Condvar::new(),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open_descriptor(&self) -> Result<File, HeavyOperationLeaseError> {
        let unsafe_file = || fail("heavy-operation lease ownership or mode is unsafe");
        if let Ok(before) = std::fs::symlink_metadata(&self.path)
            && (before.file_type().is_symlink() || !before.file_type().is_file())
        {
            return Err(fail("heavy-operation lease file cannot be opened safely"));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&self.path)
            .map_err(|_| fail("heavy-operation lease file cannot be opened safely"))?;
        let after = std::fs::symlink_metadata(&self.path).map_err(|_| unsafe_file())?;
        let row = file.metadata().map_err(|_| unsafe_file())?;
        if !row.is_file() || after.file_type().is_symlink() {
            return Err(unsafe_file());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if (row.dev(), row.ino()) != (after.dev(), after.ino())
                || crate::atomic::current_uid().is_some_and(|u| u != row.uid())
                || row.nlink() != 1
                || row.permissions().mode() & 0o077 != 0
            {
                return Err(unsafe_file());
            }
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| fail("heavy-operation lease permissions cannot be restricted"))?;
            let tightened = file.metadata().map_err(|_| unsafe_file())?;
            if tightened.permissions().mode() & 0o077 != 0 {
                return Err(fail("heavy-operation lease permissions cannot be restricted"));
            }
        }
        Ok(file)
    }


    pub fn acquire(
        &self,
        blocking: bool,
        timeout: Option<Duration>,
    ) -> Result<bool, HeavyOperationLeaseError> {
        let started = Instant::now();
        let deadline = timeout.map(|t| started + t);
        {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            while st.held {
                if !blocking {
                    return Ok(false);
                }
                match deadline {
                    None => st = self.freed.wait(st).unwrap_or_else(PoisonError::into_inner),
                    Some(d) => {
                        let now = Instant::now();
                        if now >= d {
                            return Ok(false);
                        }
                        st = self.freed.wait_timeout(st, d - now).unwrap_or_else(PoisonError::into_inner).0;
                    }
                }
            }
            st.held = true;
        }
        let outcome = self.lock_file(blocking, deadline);
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match outcome {
            Ok(Some(file)) => {
                if st.file.is_some() {
                    return Err(fail("heavy-operation lease entered an impossible double-owner state"));
                }
                st.file = Some(file);
                Ok(true)
            }
            Ok(None) => {
                st.held = false;
                self.freed.notify_one();
                Ok(false)
            }
            Err(e) => {
                st.held = false;
                self.freed.notify_one();
                Err(e)
            }
        }
    }

    fn lock_file(
        &self,
        blocking: bool,
        deadline: Option<Instant>,
    ) -> Result<Option<File>, HeavyOperationLeaseError> {
        let file = self.open_descriptor()?;
        let lease_failed = || fail("heavy-operation cross-process lease failed");
        if !blocking {
            return match file.try_lock() {
                Ok(()) => Ok(Some(file)),
                Err(TryLockError::WouldBlock) => Ok(None),
                Err(TryLockError::Error(_)) => Err(lease_failed()),
            };
        }
        match deadline {
            None => {
                file.lock().map_err(|_| lease_failed())?;
                Ok(Some(file))
            }
            Some(d) => loop {
                match file.try_lock() {
                    Ok(()) => return Ok(Some(file)),
                    Err(TryLockError::WouldBlock) => {
                        let now = Instant::now();
                        if now >= d {
                            return Ok(None);
                        }
                        std::thread::sleep((d - now).min(Duration::from_millis(10)));
                    }
                    Err(TryLockError::Error(_)) => return Err(lease_failed()),
                }
            },
        }
    }


    pub fn release(&self) -> Result<(), HeavyOperationLeaseError> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !st.held || st.file.is_none() {
            return Err(fail("release unlocked lock"));
        }
        st.file = None;
        st.held = false;
        self.freed.notify_one();
        Ok(())
    }

    #[must_use]
    pub fn locked(&self) -> bool {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).held
    }


    pub fn inheritable_stdio(&self) -> Result<Option<Stdio>, HeavyOperationLeaseError> {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &st.file {
            None => Ok(None),
            Some(f) => f
                .try_clone()
                .map(|c| Some(Stdio::from(c)))
                .map_err(|_| fail("heavy-operation lease descriptor cannot be duplicated")),
        }
    }


    pub fn guard(&self) -> Result<LeaseGuard<'_>, HeavyOperationLeaseError> {
        self.acquire(true, None)?;
        Ok(LeaseGuard { lease: self })
    }
}

#[derive(Debug)]
pub struct LeaseGuard<'a> {
    lease: &'a HeavyOperationLease,
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        let _ = self.lease.release();
    }
}

