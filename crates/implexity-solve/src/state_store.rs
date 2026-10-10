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



use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use implexity_core::error::{CaeError, CaeResult};
use implexity_io::atomic::{unique_token, fsync_directory};
use implexity_io::{fsguard, storage_budget};
use std::io::{Read, Write, BufReader, BufWriter};
use sha2::{Digest, Sha256};

pub const RAM_BUDGET_ENV: &str = "IMPLEXITY_CHECKPOINT_RAM_BUDGET_BYTES";
pub const DISK_BUDGET_ENV: &str = "IMPLEXITY_CHECKPOINT_DISK_BUDGET_BYTES";
pub const DISK_ROOT_ENV: &str = "IMPLEXITY_CHECKPOINT_DIR";
pub const DEFAULT_RAM_BUDGET_BYTES: u64 = 1 << 30;

const MAGIC: &[u8; 8] = b"IMPXSNP1";
const HEADER: usize = 8 + 8 + 8;
const DIGEST: usize = 32;

pub trait SnapshotStore: Send {


    fn put(&mut self, slot: usize, step: usize, state: &[f64]) -> CaeResult<()>;


    fn get(&self, slot: usize) -> CaeResult<(usize, Vec<f64>)>;


    fn release(&mut self, slot: usize) -> CaeResult<()>;
    fn bytes_in_use(&self) -> u64;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreBudget {
    pub ram_bytes: u64,
    pub disk_bytes: u64,
    pub disk_root: Option<PathBuf>,
}

impl Default for StoreBudget {
    fn default() -> Self {
        let root = std::env::temp_dir().join("implexity-checkpoints");
        Self { ram_bytes: storage_budget::default_memory_bytes(), disk_bytes: storage_budget::default_disk_bytes(&root), disk_root: Some(root) }
    }
}

fn parse_bytes(name: &str, value: &str) -> CaeResult<u64> {
    value.trim().parse::<u64>().map_err(|_| {
        CaeError::contract(format!("{name} must be a non-negative integer number of bytes (got {value:?})"))
    })
}

impl StoreBudget {
    #[must_use]
    pub const fn memory(ram_bytes: u64) -> Self {
        Self { ram_bytes, disk_bytes: 0, disk_root: None }
    }

    #[must_use]
    pub const fn state_bytes(len: usize) -> u64 {
        (len as u64).saturating_mul(8)
    }



    pub fn from_environment() -> CaeResult<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }



    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> CaeResult<Self> {
        let ram_bytes = match lookup(RAM_BUDGET_ENV) {
            Some(v) => parse_bytes(RAM_BUDGET_ENV, &v)?,
            None => storage_budget::default_memory_bytes(),
        };
        let declared_root = lookup(DISK_ROOT_ENV).filter(|v| !v.is_empty()).map(PathBuf::from);
        let explicit_disk = lookup(DISK_BUDGET_ENV);
        if explicit_disk.is_some() && declared_root.is_none()
            && parse_bytes(DISK_BUDGET_ENV, explicit_disk.as_deref().unwrap_or("0"))? > 0 {
            return Err(CaeError::contract(format!("{DISK_BUDGET_ENV} is positive but {DISK_ROOT_ENV} is not set")));
        }
        let disk_root = declared_root.or_else(|| Some(std::env::temp_dir().join("implexity-checkpoints")));
        let disk_bytes = match explicit_disk {
            Some(v) => parse_bytes(DISK_BUDGET_ENV, &v)?,
            None => disk_root.as_deref().map_or(0, storage_budget::default_disk_bytes),
        };
        Ok(Self { ram_bytes, disk_bytes, disk_root })
    }

    #[must_use]
    pub fn environment(&self) -> Vec<(String, String)> {
        let mut out = vec![
            (RAM_BUDGET_ENV.to_string(), self.ram_bytes.to_string()),
            (DISK_BUDGET_ENV.to_string(), self.disk_bytes.to_string()),
        ];
        if let Some(root) = &self.disk_root {
            out.push((DISK_ROOT_ENV.to_string(), root.display().to_string()));
        }
        out
    }
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    budget: u64,
    slots: BTreeMap<usize, (usize, Vec<f64>)>,
    bytes: u64,
    peak: u64,
}

impl MemoryStore {
    #[must_use]
    pub fn new(budget_bytes: u64) -> Self {
        Self { budget: budget_bytes, slots: BTreeMap::new(), bytes: 0, peak: 0 }
    }

    #[must_use]
    pub const fn peak_bytes(&self) -> u64 {
        self.peak
    }
}

impl SnapshotStore for MemoryStore {
    fn put(&mut self, slot: usize, step: usize, state: &[f64]) -> CaeResult<()> {
        let new = StoreBudget::state_bytes(state.len());
        let old = self.slots.get(&slot).map_or(0, |(_, s)| StoreBudget::state_bytes(s.len()));
        let total = self.bytes - old + new;
        if total > self.budget {
            return Err(CaeError::contract(format!(
                "checkpoint snapshot of step {step} ({new} bytes) exceeds the RAM checkpoint budget ({} of {} bytes in use)",
                self.bytes, self.budget
            )));
        }
        match self.slots.get_mut(&slot) {

            Some((s, v)) if v.len() == state.len() => {
                *s = step;
                v.copy_from_slice(state);
            }
            _ => {
                self.slots.insert(slot, (step, state.to_vec()));
            }
        }
        self.bytes = total;
        self.peak = self.peak.max(total);
        Ok(())
    }

    fn get(&self, slot: usize) -> CaeResult<(usize, Vec<f64>)> {
        self.slots
            .get(&slot)
            .map(|(step, state)| (*step, state.clone()))
            .ok_or_else(|| CaeError::contract(format!("checkpoint slot {slot} is empty")))
    }

    fn release(&mut self, slot: usize) -> CaeResult<()> {
        if let Some((_, state)) = self.slots.remove(&slot) {
            self.bytes -= StoreBudget::state_bytes(state.len());
        }
        Ok(())
    }

    fn bytes_in_use(&self) -> u64 {
        self.bytes
    }
}

fn io_error(what: &str, path: &Path, e: impl std::fmt::Display) -> CaeError {
    CaeError::contract(format!("checkpoint {what} failed for {}: {e}", path.display()))
}

#[derive(Debug)]
pub struct DiskStore {
    dir: PathBuf,
    budget: u64,
    slots: BTreeMap<usize, (usize, usize, [u8; DIGEST])>,
    bytes: u64,
    peak: u64,
    writes: usize,
    persistent: bool,
    persisted_slots: std::collections::BTreeSet<usize>,
    persisted_identity: Option<String>,
}

impl DiskStore {


    pub fn new(root: &Path, budget_bytes: u64) -> CaeResult<Self> {
        std::fs::create_dir_all(root).map_err(|e| io_error("directory creation", root, e))?;
        let dir = root.join(format!("snapshots-{}", unique_token()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir).map_err(|e| io_error("directory creation", &dir, e))?;
        Ok(Self { dir, budget: budget_bytes, slots: BTreeMap::new(), bytes: 0, peak: 0, writes: 0, persistent: false, persisted_slots: Default::default(), persisted_identity: None })
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.dir
    }

    #[must_use]
    pub const fn peak_bytes(&self) -> u64 {
        self.peak
    }

    #[must_use]
    pub const fn writes(&self) -> usize {
        self.writes
    }

    fn path(&self, slot: usize) -> PathBuf {
        self.dir.join(format!("slot-{slot}.snap"))
    }
}

impl SnapshotStore for DiskStore {
    fn put(&mut self, slot: usize, step: usize, state: &[f64]) -> CaeResult<()> {
        if self.persisted_slots.contains(&slot) { return Err(CaeError::contract("published persistent snapshot cannot be overwritten")); }
        let new = StoreBudget::state_bytes(state.len());
        let old = self.slots.get(&slot).map_or(0, |(_, len, _)| StoreBudget::state_bytes(*len));
        let total = self.bytes - old + new;
        if total > self.budget {
            return Err(CaeError::contract(format!(
                "checkpoint snapshot of step {step} ({new} bytes) exceeds the disk checkpoint budget ({} of {} bytes in use)",
                self.bytes, self.budget
            )));
        }
        let path = self.path(slot);
        let temporary = self.dir.join(format!(".slot-{slot}-{}.tmp", unique_token()));
        let result = (|| -> CaeResult<[u8; DIGEST]> {
            let file = fsguard::create_new_nofollow(&temporary, 0o600).map_err(|e| io_error("write", &temporary, e))?;
            let mut writer = BufWriter::with_capacity(65536, file);
            let mut hash = Sha256::new();
            for part in [MAGIC.as_slice(), &(step as u64).to_le_bytes(), &(state.len() as u64).to_le_bytes()] {
                writer.write_all(part).map_err(|e| io_error("write", &temporary, e))?;
                hash.update(part);
            }
            for value in state {
                let bytes = value.to_le_bytes();
                writer.write_all(&bytes).map_err(|e| io_error("write", &temporary, e))?;
                hash.update(bytes);
            }
            let digest: [u8; DIGEST] = hash.finalize().into();
            writer.write_all(&digest).and_then(|()| writer.flush()).map_err(|e| io_error("write", &temporary, e))?;
            writer.get_ref().sync_all().map_err(|e| io_error("sync", &temporary, e))?;
            drop(writer);
            std::fs::rename(&temporary, &path).map_err(|e| io_error("publish", &path, e))?;
            fsync_directory(&self.dir).map_err(|e| io_error("sync", &self.dir, e))?;
            Ok(digest)
        })();
        if result.is_err() { let _ = std::fs::remove_file(&temporary); }
        let digest = result?;
        self.slots.insert(slot, (step, state.len(), digest));
        self.bytes = total;
        self.peak = self.peak.max(total);
        self.writes += 1;
        Ok(())
    }

    fn get(&self, slot: usize) -> CaeResult<(usize, Vec<f64>)> {
        let Some(&(step, len, digest)) = self.slots.get(&slot) else {
            return Err(CaeError::contract(format!("checkpoint slot {slot} is empty")));
        };
        let path = self.path(slot);
        let corrupt = || CaeError::contract(format!("checkpoint snapshot record {} failed its integrity check", path.display()));
        let file = fsguard::open_nofollow(&path).map_err(|e| io_error("read", &path, e))?;
        let before = fsguard::stat_file(&file).map_err(|e| io_error("read", &path, e))?;
        let size = (HEADER as u64).checked_add(StoreBudget::state_bytes(len)).and_then(|n| n.checked_add(DIGEST as u64)).ok_or_else(corrupt)?;
        if !before.is_owned_single_regular() || before.size != size { return Err(corrupt()); }
        let mut reader = BufReader::with_capacity(65536, file);
        let mut header = [0u8; HEADER];
        reader.read_exact(&mut header).map_err(|e| io_error("read", &path, e))?;
        let word = |bytes: &[u8]| { let mut word = [0; 8]; word.copy_from_slice(bytes); u64::from_le_bytes(word) };
        if &header[..8] != MAGIC || word(&header[8..16]) != step as u64 || word(&header[16..24]) != len as u64 { return Err(corrupt()); }
        let mut hash = Sha256::new(); hash.update(header);
        let mut state = Vec::new();
        state.try_reserve_exact(len).map_err(|e| io_error("allocation", &path, e))?;
        for _ in 0..len {
            let mut bytes = [0;8]; reader.read_exact(&mut bytes).map_err(|e| io_error("read", &path, e))?;
            hash.update(bytes); state.push(f64::from_bits(u64::from_le_bytes(bytes)));
        }
        let mut stored = [0; DIGEST]; reader.read_exact(&mut stored).map_err(|e| io_error("read", &path, e))?;
        let actual: [u8;DIGEST] = hash.finalize().into();
        let after = fsguard::stat_file(reader.get_ref()).map_err(|e| io_error("read", &path, e))?;
        let current = fsguard::stat_nofollow(&path).map_err(|e| io_error("read", &path, e))?;
        if stored != digest || actual != digest || before.data_identity() != after.data_identity() || !before.same_object(&current) { return Err(corrupt()); }
        Ok((step, state))
    }

    fn release(&mut self, slot: usize) -> CaeResult<()> {
        if self.persisted_slots.contains(&slot) { return Err(CaeError::contract("published persistent snapshot cannot be removed")); }
        if let Some((_, len, _)) = self.slots.get(&slot) {
            let bytes = StoreBudget::state_bytes(*len);
            let path = self.path(slot);
            std::fs::remove_file(&path).map_err(|e| io_error("removal", &path, e))?;
            self.slots.remove(&slot);
            self.bytes -= bytes;
        }
        Ok(())
    }

    fn bytes_in_use(&self) -> u64 {
        self.bytes
    }
}


#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistentSnapshotManifest {
    schema: String,
    identity: String,
    slots: BTreeMap<usize, (usize, usize, [u8; DIGEST])>,
}

impl DiskStore {
    pub fn persist_selected(&mut self, identity: &str, selected: &[usize]) -> CaeResult<()> {
        if identity.is_empty() || selected.is_empty() || self.persisted_identity.as_ref().is_some_and(|old| old != identity) {
            return Err(CaeError::contract("persistent snapshots require identity and slots"));
        }
        let mut slots = BTreeMap::new();
        for &slot in selected {
            let value = *self.slots.get(&slot).ok_or_else(|| CaeError::contract("persistent snapshot slot is absent"))?;
            if slots.insert(slot, value).is_some() { return Err(CaeError::contract("persistent snapshot slot is repeated")); }
        }
        let manifest = PersistentSnapshotManifest { schema: "implexity-history-snapshots/1".into(), identity: identity.into(), slots };
        let bytes = serde_json::to_vec(&manifest).map_err(|e| CaeError::contract(e.to_string()))?;
        implexity_io::atomic::write_atomic(&self.dir.join("manifest.json"), &bytes)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        self.persistent = true;
        self.persisted_identity = Some(identity.into());
        self.persisted_slots = manifest.slots.keys().copied().collect();
        let removed: Vec<_> = self.slots.keys().copied().filter(|slot| !manifest.slots.contains_key(slot)).collect();
        for slot in removed { self.release(slot)?; }
        fsync_directory(&self.dir).map_err(|e| io_error("sync", &self.dir, e))?;
        Ok(())
    }

    pub fn open_persistent(directory: &Path, identity: &str, budget_bytes: u64, maximum_slots: usize, maximum_state_len: usize) -> CaeResult<Self> {
        let status = fsguard::stat_nofollow(directory).map_err(|e| io_error("inspection", directory, e))?;
        if !status.is_dir() || status.is_symlink() || !status.owned || !status.owner_only || identity.is_empty() || maximum_slots == 0 || maximum_state_len == 0 {
            return Err(CaeError::contract("persistent snapshot directory or limits are invalid"));
        }
        let path = directory.join("manifest.json");
        let file = fsguard::open_nofollow(&path).map_err(|e| io_error("read", &path, e))?;
        let before = fsguard::stat_file(&file).map_err(|e| io_error("read", &path, e))?;
        let max_bytes = maximum_slots.checked_mul(512).and_then(|n| n.checked_add(identity.len())).and_then(|n| n.checked_add(512))
            .ok_or_else(|| CaeError::contract("persistent manifest limit overflow"))?;
        if !before.is_owned_single_regular() || before.size > max_bytes as u64 { return Err(CaeError::contract("persistent manifest size or ownership is invalid")); }
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        reader.by_ref().take(max_bytes as u64 + 1).read_to_end(&mut bytes).map_err(|e| io_error("read", &path, e))?;
        let after = fsguard::stat_file(reader.get_ref()).map_err(|e| io_error("read", &path, e))?;
        let current = fsguard::stat_nofollow(&path).map_err(|e| io_error("read", &path, e))?;
        if bytes.len() > max_bytes || before.data_identity() != after.data_identity() || !before.same_object(&current) {
            return Err(CaeError::contract("persistent manifest changed during read"));
        }
        let manifest: PersistentSnapshotManifest = serde_json::from_slice(&bytes).map_err(|e| CaeError::contract(e.to_string()))?;
        if manifest.schema != "implexity-history-snapshots/1" || manifest.identity != identity || manifest.slots.is_empty() || manifest.slots.len() > maximum_slots {
            return Err(CaeError::contract("persistent snapshot identity or slot count differs"));
        }
        let mut total = 0u64;
        for (_, len, _) in manifest.slots.values() {
            if *len == 0 || *len > maximum_state_len { return Err(CaeError::contract("persistent snapshot dimension exceeds its limit")); }
            total = total.checked_add((*len as u64).checked_mul(8).ok_or_else(|| CaeError::contract("persistent snapshot size overflow"))?)
                .ok_or_else(|| CaeError::contract("persistent snapshot total overflow"))?;
        }
        if total > budget_bytes { return Err(CaeError::contract("persistent snapshots exceed the disk budget")); }
        let persisted_slots = manifest.slots.keys().copied().collect();
        let store = Self { dir: directory.into(), budget: budget_bytes, slots: manifest.slots, bytes: total, peak: total, writes: 0, persistent: true, persisted_slots, persisted_identity: Some(identity.into()) };
        for &slot in store.slots.keys() {
            let (_, state) = store.get(slot)?;
            if state.iter().any(|v| !v.is_finite()) { return Err(CaeError::contract("persistent snapshot is nonfinite")); }
        }
        Ok(store)
    }

    pub fn snapshot_steps(&self) -> Vec<(usize, usize)> {
        self.slots.iter().map(|(&slot, &(step, _, _))| (slot, step)).collect()
    }
}

impl Drop for DiskStore {
    fn drop(&mut self) {
        if !self.persistent { let _ = std::fs::remove_dir_all(&self.dir); }
    }
}

#[derive(Debug)]
pub struct TieredStore {
    ram_slots: usize,
    disk_slots: usize,
    memory: MemoryStore,
    disk: Option<DiskStore>,
}

impl TieredStore {


    pub fn new(
        budget: &StoreBudget,
        ram_slots: usize,
        disk_slots: usize,
        state_len: usize,
    ) -> CaeResult<Self> {
        let one = StoreBudget::state_bytes(state_len);
        let ram_need = one.saturating_mul(ram_slots as u64);
        if ram_need > budget.ram_bytes {
            return Err(CaeError::contract(format!(
                "{ram_slots} RAM checkpoint snapshots of {one} bytes need {ram_need} bytes but the RAM checkpoint budget is {} bytes",
                budget.ram_bytes
            )));
        }
        let disk = if disk_slots > 0 {
            let disk_need = one.saturating_mul(disk_slots as u64);
            if disk_need > budget.disk_bytes {
                return Err(CaeError::contract(format!(
                    "{disk_slots} disk checkpoint snapshots of {one} bytes need {disk_need} bytes but the disk checkpoint budget is {} bytes",
                    budget.disk_bytes
                )));
            }
            let Some(root) = &budget.disk_root else {
                return Err(CaeError::contract("disk checkpoint snapshots need a checkpoint directory"));
            };
            Some(DiskStore::new(root, budget.disk_bytes)?)
        } else {
            None
        };
        Ok(Self { ram_slots, disk_slots, memory: MemoryStore::new(budget.ram_bytes), disk })
    }

    #[must_use]
    pub const fn ram_slots(&self) -> usize {
        self.ram_slots
    }

    #[must_use]
    pub const fn disk_slots(&self) -> usize {
        self.disk_slots
    }

    #[must_use]
    pub fn peak_bytes(&self) -> (u64, u64) {
        (self.memory.peak_bytes(), self.disk.as_ref().map_or(0, DiskStore::peak_bytes))
    }

    #[must_use]
    pub fn disk_writes(&self) -> usize {
        self.disk.as_ref().map_or(0, DiskStore::writes)
    }

    fn tier(&self, slot: usize) -> CaeResult<Option<usize>> {
        if slot < self.ram_slots {
            Ok(None)
        } else if slot < self.ram_slots + self.disk_slots {
            Ok(Some(slot - self.ram_slots))
        } else {
            Err(CaeError::contract(format!(
                "checkpoint slot {slot} does not exist ({} RAM and {} disk slots)",
                self.ram_slots, self.disk_slots
            )))
        }
    }

    fn disk_mut(&mut self) -> CaeResult<&mut DiskStore> {
        self.disk.as_mut().ok_or_else(|| CaeError::contract("the checkpoint store has no disk tier"))
    }
}

impl SnapshotStore for TieredStore {
    fn put(&mut self, slot: usize, step: usize, state: &[f64]) -> CaeResult<()> {
        match self.tier(slot)? {
            None => self.memory.put(slot, step, state),
            Some(k) => self.disk_mut()?.put(k, step, state),
        }
    }

    fn get(&self, slot: usize) -> CaeResult<(usize, Vec<f64>)> {
        match self.tier(slot)? {
            None => self.memory.get(slot),
            Some(k) => self
                .disk
                .as_ref()
                .ok_or_else(|| CaeError::contract("the checkpoint store has no disk tier"))?
                .get(k),
        }
    }

    fn release(&mut self, slot: usize) -> CaeResult<()> {
        match self.tier(slot)? {
            None => self.memory.release(slot),
            Some(k) => self.disk_mut()?.release(k),
        }
    }

    fn bytes_in_use(&self) -> u64 {
        self.memory.bytes_in_use() + self.disk.as_ref().map_or(0, SnapshotStore::bytes_in_use)
    }
}


#[derive(Debug)]
pub struct AdaptiveStore {
    budget: StoreBudget,
    memory: MemoryStore,
    disk: Option<DiskStore>,
    locations: BTreeMap<usize, (bool, u64)>,
}

impl AdaptiveStore {
    #[must_use]
    pub fn new(budget: &StoreBudget) -> Self {
        Self { budget: budget.clone(), memory: MemoryStore::new(budget.ram_bytes), disk: None, locations: BTreeMap::new() }
    }

    #[must_use]
    pub fn peak_bytes(&self) -> (u64, u64) {
        (self.memory.peak_bytes(), self.disk.as_ref().map_or(0, DiskStore::peak_bytes))
    }

    #[must_use]
    pub fn disk_writes(&self) -> usize { self.disk.as_ref().map_or(0, DiskStore::writes) }
}

impl SnapshotStore for AdaptiveStore {
    fn put(&mut self, slot: usize, step: usize, state: &[f64]) -> CaeResult<()> {
        let old = self.locations.get(&slot).copied();
        let old_ram = old.filter(|(disk,_)| !disk).map_or(0, |(_,bytes)| bytes);
        let bytes = StoreBudget::state_bytes(state.len());
        if old.is_none_or(|(disk,_)| !disk) && self.memory.bytes_in_use().saturating_sub(old_ram).saturating_add(bytes) <= self.budget.ram_bytes {
            self.memory.put(slot, step, state)?;
            self.locations.insert(slot, (false, bytes));
            return Ok(());
        }
        if self.disk.is_none() {
            let root = self.budget.disk_root.as_deref().filter(|_| self.budget.disk_bytes > 0)
                .ok_or_else(|| CaeError::contract(format!("checkpoint step {step} needs {bytes} bytes after {} RAM bytes; disk spill is disabled by the checkpoint budget", self.memory.bytes_in_use())))?;
            self.disk = Some(DiskStore::new(root, self.budget.disk_bytes)?);
        }
        self.disk.as_mut().ok_or_else(|| CaeError::contract("checkpoint disk store unavailable"))?.put(slot, step, state)?;
        if old.is_some_and(|(disk,_)| !disk) { self.memory.release(slot)?; }
        self.locations.insert(slot, (true, bytes));
        Ok(())
    }

    fn get(&self, slot: usize) -> CaeResult<(usize, Vec<f64>)> {
        match self.locations.get(&slot) {
            Some((false,_)) => self.memory.get(slot),
            Some((true,_)) => self.disk.as_ref().ok_or_else(|| CaeError::contract("checkpoint disk store unavailable"))?.get(slot),
            None => Err(CaeError::contract(format!("checkpoint slot {slot} is empty"))),
        }
    }

    fn release(&mut self, slot: usize) -> CaeResult<()> {
        match self.locations.get(&slot) {
            Some((false,_)) => self.memory.release(slot)?,
            Some((true,_)) => self.disk.as_mut().ok_or_else(|| CaeError::contract("checkpoint disk store unavailable"))?.release(slot)?,
            None => return Ok(()),
        }
        self.locations.remove(&slot);
        Ok(())
    }

    fn bytes_in_use(&self) -> u64 {
        self.memory.bytes_in_use().saturating_add(self.disk.as_ref().map_or(0, SnapshotStore::bytes_in_use))
    }
}
