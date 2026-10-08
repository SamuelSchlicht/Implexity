// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Map, Value};

pub const GENERIC_RUNTIME_PHASES: [&str; 6] =
    ["created", "finalizing", "initializing", "running", "stopping", "terminal"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ResourceTelemetryError(pub String);

type TelemetryResult<T> = Result<T, ResourceTelemetryError>;

fn err<T>(m: &str) -> TelemetryResult<T> {
    Err(ResourceTelemetryError(m.into()))
}


pub fn require_generic_phase(value: &str) -> TelemetryResult<&str> {
    if GENERIC_RUNTIME_PHASES.contains(&value) {
        Ok(value)
    } else {
        err("runtime phase is not in the generic phase set")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcessBirthIdentity {
    pid: i64,
    parent_pid: i64,
    process_group_id: i64,
    birth_marker: String,
}

impl ProcessBirthIdentity {

    pub fn new(
        pid: i64,
        parent_pid: i64,
        process_group_id: i64,
        birth_marker: &str,
    ) -> TelemetryResult<Self> {
        for (name, value) in
            [("pid", pid), ("parent_pid", parent_pid), ("process_group_id", process_group_id)]
        {
            if value < 0 {
                return Err(ResourceTelemetryError(format!("{name} must be a non-negative integer")));
            }
        }
        if pid == 0 || process_group_id == 0 {
            return err("process and process-group identifiers must be positive");
        }
        if birth_marker.is_empty() || birth_marker != birth_marker.trim() {
            return err("birth marker must be a canonical string");
        }
        Ok(Self { pid, parent_pid, process_group_id, birth_marker: birth_marker.to_string() })
    }

    #[must_use]
    pub fn pid(&self) -> i64 {
        self.pid
    }

    #[must_use]
    pub fn parent_pid(&self) -> i64 {
        self.parent_pid
    }

    #[must_use]
    pub fn process_group_id(&self) -> i64 {
        self.process_group_id
    }

    #[must_use]
    pub fn birth_marker(&self) -> &str {
        &self.birth_marker
    }

    #[must_use]
    pub fn stable_key(&self) -> (i64, String) {
        (self.pid, self.birth_marker.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessObservation {
    pub identity: ProcessBirthIdentity,
    pub rss_bytes: i64,
    pub footprint_bytes: Option<i64>,
}

impl ProcessObservation {

    pub fn new(
        identity: ProcessBirthIdentity,
        rss_bytes: i64,
        footprint_bytes: Option<i64>,
    ) -> TelemetryResult<Self> {
        if rss_bytes < 0 {
            return err("rss_bytes must be a non-negative integer");
        }
        if footprint_bytes.is_some_and(|f| f < 0) {
            return err("footprint_bytes must be a non-negative integer or null");
        }
        Ok(Self { identity, rss_bytes, footprint_bytes })
    }

    #[must_use]
    pub fn budgeted_memory_bytes(&self) -> i64 {
        self.footprint_bytes.map_or(self.rss_bytes, |f| self.rss_bytes.max(f))
    }
}

#[cfg(not(target_os = "macos"))]
fn page_size() -> Option<i64> {
    let raw = std::fs::read("/proc/self/auxv").ok()?;
    let width = size_of::<usize>();
    for pair in raw.chunks_exact(2 * width) {
        let (k, v) = pair.split_at(width);
        let key = usize::from_ne_bytes(k.try_into().ok()?);
        let value = usize::from_ne_bytes(v.try_into().ok()?);
        if key == 6 {
            return i64::try_from(value).ok();
        }
        if key == 0 {
            break;
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn observe_linux(pid: i64, page_size: i64) -> Option<ProcessObservation> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let closing = stat.rfind(')')?;
    let fields: Vec<&str> = stat.get(closing + 2..)?.split_whitespace().collect();
    let parent_pid: i64 = fields.get(1)?.parse().ok()?;
    let process_group_id: i64 = fields.get(2)?.parse().ok()?;
    if process_group_id <= 0 {
        return None;
    }
    let birth = fields.get(19)?;
    let rss_pages: i64 = fields.get(21)?.parse().ok()?;
    let rss = rss_pages.max(0).saturating_mul(page_size);
    let identity =
        ProcessBirthIdentity::new(pid, parent_pid, process_group_id, &format!("linux:{birth}")).ok()?;
    ProcessObservation::new(identity, rss, None).ok()
}

#[cfg(target_os = "macos")]
mod darwin {
    use libproc::bsd_info::BSDInfo;
    use libproc::pid_rusage::{RUsageInfoV0, pidrusage};
    use libproc::proc_pid::pidinfo;
    use libproc::processes::{ProcFilter, pids_by_type};
    use libproc::task_info::TaskInfo;

    use super::{ProcessBirthIdentity, ProcessObservation, ResourceTelemetryError, TelemetryResult, err};

    fn bsd_identity(pid: i32) -> Option<(i64, i64, i64, u64, u64)> {
        let bsd = pidinfo::<BSDInfo>(pid, 0).ok()?;
        if i64::from(bsd.pbi_pid) != i64::from(pid) {
            return None;
        }
        Some((
            i64::from(bsd.pbi_pid),
            i64::from(bsd.pbi_ppid),
            i64::from(bsd.pbi_pgid),
            bsd.pbi_start_tvsec,
            bsd.pbi_start_tvusec,
        ))
    }


    pub(super) fn observe(pid: i64, include_rss: bool) -> TelemetryResult<Option<ProcessObservation>> {
        let Ok(pid) = i32::try_from(pid) else { return Ok(None) };
        let Some((_, parent_pid, process_group_id, start_s, start_us)) = bsd_identity(pid) else {
            return Ok(None);
        };
        let mut rss = 0_u64;
        let mut footprint = None;
        if include_rss {
            let task = pidinfo::<TaskInfo>(pid, 0);
            let usage = pidrusage::<RUsageInfoV0>(pid);
            match bsd_identity(pid) {
                Some((_, _, _, after_s, after_us)) if (after_s, after_us) == (start_s, start_us) => {}
                _ => return Ok(None),
            }
            let (Ok(task), Ok(usage)) = (task, usage) else {
                return err("Darwin birth-bound RSS/physical-footprint observation failed");
            };
            rss = task.pti_resident_size.max(usage.ri_resident_size);
            footprint = Some(i64::try_from(usage.ri_phys_footprint).unwrap_or(i64::MAX));
        }
        let Ok(identity) = ProcessBirthIdentity::new(
            i64::from(pid),
            parent_pid,
            process_group_id,
            &format!("darwin:{start_s}:{start_us}"),
        ) else {
            return Ok(None);
        };
        ProcessObservation::new(identity, i64::try_from(rss).unwrap_or(i64::MAX), footprint).map(Some)
    }

    fn list(filter: ProcFilter, failed: &str, changed: &str) -> TelemetryResult<Vec<i64>> {
        for _ in 0..2 {

            errno::set_errno(errno::Errno(0));
            let pids = pids_by_type(filter).map_err(|_| ResourceTelemetryError(failed.into()))?;
            if pids.is_empty() || pids.len() < pids.capacity() {
                return Ok(pids.into_iter().filter(|&p| p > 0).map(i64::from).collect());
            }
        }
        Err(ResourceTelemetryError(changed.into()))
    }

    pub(super) fn all_pids() -> TelemetryResult<Vec<i64>> {
        let pids = list(
            ProcFilter::All,
            "Darwin process enumeration failed",
            "process table changed during enumeration",
        )?;
        if pids.is_empty() {
            return err("Darwin process enumeration failed");
        }
        Ok(pids)
    }

    fn filtered(filter: ProcFilter) -> TelemetryResult<Vec<i64>> {
        list(
            filter,
            "Darwin filtered process enumeration failed",
            "filtered process table changed during enumeration",
        )
    }

    pub(super) fn group_pids(process_group_id: i64) -> TelemetryResult<Vec<i64>> {
        let Ok(pgrpid) = u32::try_from(process_group_id) else { return Ok(Vec::new()) };
        filtered(ProcFilter::ByProgramGroup { pgrpid })
    }

    pub(super) fn child_pids(parent_pid: i64) -> TelemetryResult<Vec<i64>> {
        let Ok(ppid) = u32::try_from(parent_pid) else { return Ok(Vec::new()) };
        filtered(ProcFilter::ByParentProcess { ppid })
    }
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct OrderedObservations {
    rows: Vec<ProcessObservation>,
    position: BTreeMap<(i64, String), usize>,
}

#[cfg(target_os = "macos")]
impl OrderedObservations {
    fn contains(&self, identity: &ProcessBirthIdentity) -> bool {
        self.position.contains_key(&identity.stable_key())
    }

    fn put(&mut self, observed: ProcessObservation) {
        let key = observed.identity.stable_key();
        if let Some(&at) = self.position.get(&key)
            && let Some(slot) = self.rows.get_mut(at)
        {
            *slot = observed;
        } else {
            self.position.insert(key, self.rows.len());
            self.rows.push(observed);
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Platform {
    #[cfg(not(target_os = "macos"))]
    Linux { page_size: i64 },
    #[cfg(target_os = "macos")]
    Darwin,
}

#[derive(Debug, Clone)]
pub struct ProcessTableReader {
    platform: Platform,
}

impl ProcessTableReader {

    pub fn new() -> TelemetryResult<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self { platform: Platform::Darwin })
        }
        #[cfg(not(target_os = "macos"))]
        {
            if !cfg!(target_os = "linux") {
                return err("birth-bound process inspection is unsupported");
            }
            let Some(page_size) = page_size() else {
                return err("birth-bound process inspection is unsupported");
            };
            Ok(Self { platform: Platform::Linux { page_size } })
        }
    }

    #[must_use]
    pub fn is_darwin(&self) -> bool {
        match self.platform {
            #[cfg(not(target_os = "macos"))]
            Platform::Linux { .. } => false,
            #[cfg(target_os = "macos")]
            Platform::Darwin => true,
        }
    }


    pub fn observe(&self, pid: i64) -> TelemetryResult<Option<ProcessObservation>> {
        if pid <= 0 {
            return err("pid must be a positive integer");
        }
        match self.platform {
            #[cfg(not(target_os = "macos"))]
            Platform::Linux { page_size } => Ok(observe_linux(pid, page_size)),
            #[cfg(target_os = "macos")]
            Platform::Darwin => darwin::observe(pid, true),
        }
    }


    pub fn snapshot(&self) -> TelemetryResult<Vec<ProcessObservation>> {
        match self.platform {
            #[cfg(not(target_os = "macos"))]
            Platform::Linux { page_size } => {
                let entries = std::fs::read_dir("/proc")
                    .map_err(|_| ResourceTelemetryError("Linux process enumeration failed".into()))?;
                let mut out = Vec::new();
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if !name.is_empty()
                        && name.bytes().all(|b| b.is_ascii_digit())
                        && let Ok(pid) = name.parse::<i64>()
                        && let Some(observed) = observe_linux(pid, page_size)
                    {
                        out.push(observed);
                    }
                }
                Ok(out)
            }
            #[cfg(target_os = "macos")]
            Platform::Darwin => {
                let mut out = Vec::new();
                for pid in darwin::all_pids()? {
                    if let Some(observed) = darwin::observe(pid, false)? {
                        out.push(observed);
                    }
                }
                Ok(out)
            }
        }
    }


    pub fn process_group_snapshot(&self, process_group_id: i64) -> TelemetryResult<Vec<ProcessObservation>> {
        if process_group_id <= 0 {
            return err("process_group_id must be a positive integer");
        }
        match self.platform {
            #[cfg(not(target_os = "macos"))]
            Platform::Linux { .. } => Ok(self
                .snapshot()?
                .into_iter()
                .filter(|r| r.identity.process_group_id == process_group_id)
                .collect()),
            #[cfg(target_os = "macos")]
            Platform::Darwin => {
                let mut out = Vec::new();
                for pid in darwin::group_pids(process_group_id)? {
                    if let Some(observed) = self.observe(pid)?
                        && observed.identity.process_group_id == process_group_id
                    {
                        out.push(observed);
                    }
                }
                Ok(out)
            }
        }
    }


    pub fn owned_snapshot(
        &self,
        root: &ProcessBirthIdentity,
        tracked: &[ProcessBirthIdentity],
    ) -> TelemetryResult<Vec<ProcessObservation>> {
        match self.platform {
            #[cfg(not(target_os = "macos"))]
            Platform::Linux { .. } => self.owned_snapshot_from_table(root, tracked),
            #[cfg(target_os = "macos")]
            Platform::Darwin => self.owned_snapshot_darwin(root, tracked),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn owned_snapshot_from_table(
        &self,
        root: &ProcessBirthIdentity,
        tracked: &[ProcessBirthIdentity],
    ) -> TelemetryResult<Vec<ProcessObservation>> {
        let snapshot = self.snapshot()?;
        let by_key: BTreeMap<(i64, String), &ProcessObservation> =
            snapshot.iter().map(|r| (r.identity.stable_key(), r)).collect();
        let mut seeds: Vec<&ProcessObservation> = Vec::new();
        if let Some(current_root) = by_key.get(&root.stable_key()) {
            seeds.push(current_root);
            seeds.extend(snapshot.iter().filter(|r| r.identity.process_group_id == root.process_group_id));
        }
        seeds.extend(tracked.iter().filter_map(|t| by_key.get(&t.stable_key()).copied()));
        let mut children: BTreeMap<i64, Vec<&ProcessObservation>> = BTreeMap::new();
        for row in &snapshot {
            children.entry(row.identity.parent_pid).or_default().push(row);
        }
        let mut found: Vec<&ProcessObservation> = Vec::new();
        let mut keys: BTreeSet<(i64, String)> = BTreeSet::new();
        for s in &seeds {
            if keys.insert(s.identity.stable_key()) {
                found.push(s);
            }
        }
        let mut pending: Vec<&ProcessObservation> = seeds;
        while let Some(parent) = pending.pop() {
            for child in children.get(&parent.identity.pid).into_iter().flatten() {
                if keys.insert(child.identity.stable_key()) {
                    found.push(child);
                    pending.push(child);
                }
            }
        }
        Ok(found.into_iter().cloned().collect())
    }

    #[cfg(target_os = "macos")]
    fn owned_snapshot_darwin(
        &self,
        root: &ProcessBirthIdentity,
        tracked: &[ProcessBirthIdentity],
    ) -> TelemetryResult<Vec<ProcessObservation>> {
        let mut found = OrderedObservations::default();
        let mut pending: Vec<ProcessObservation> = Vec::new();
        if let Some(current_root) = self.observe(root.pid)?
            && current_root.identity.stable_key() == root.stable_key()
        {
            found.put(current_root.clone());
            pending.push(current_root);
            for pid in darwin::group_pids(root.process_group_id)? {
                if let Some(observed) = self.observe(pid)? {
                    found.put(observed.clone());
                    pending.push(observed);
                }
            }
        }
        for identity in tracked {
            if let Some(observed) = self.observe(identity.pid)?
                && observed.identity.stable_key() == identity.stable_key()
            {
                found.put(observed.clone());
                pending.push(observed);
            }
        }
        let mut expanded: BTreeSet<(i64, String)> = BTreeSet::new();
        while let Some(parent) = pending.pop() {
            if !expanded.insert(parent.identity.stable_key()) {
                continue;
            }
            for pid in darwin::child_pids(parent.identity.pid)? {
                if let Some(observed) = self.observe(pid)?
                    && !found.contains(&observed.identity)
                {
                    found.put(observed.clone());
                    pending.push(observed);
                }
            }
        }
        Ok(found.rows)
    }

    #[must_use]
    pub fn still_same(identity: &ProcessBirthIdentity, observations: &[ProcessObservation]) -> bool {
        observations.iter().any(|o| o.identity.stable_key() == identity.stable_key())
    }

    #[must_use]
    pub fn descendants_of(
        root: &ProcessBirthIdentity,
        observations: &[ProcessObservation],
    ) -> Vec<ProcessObservation> {
        let current = observations.iter().rev().find(|r| r.identity.pid == root.pid);
        if current.is_none_or(|c| c.identity.stable_key() != root.stable_key()) {
            return Vec::new();
        }
        let mut children: BTreeMap<i64, Vec<&ProcessObservation>> = BTreeMap::new();
        for row in observations {
            children.entry(row.identity.parent_pid).or_default().push(row);
        }
        let mut result = Vec::new();
        let mut pending: Vec<&ProcessObservation> = children.get(&root.pid).cloned().unwrap_or_default();
        let mut seen: BTreeSet<(i64, String)> = std::iter::once(root.stable_key()).collect();
        while let Some(row) = pending.pop() {
            if !seen.insert(row.identity.stable_key()) {
                continue;
            }
            result.push(row.clone());
            pending.extend(children.get(&row.identity.pid).into_iter().flatten().copied());
        }
        result
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResourceSample {
    pub sequence: i64,
    pub elapsed_s: f64,
    pub monotonic_ns: i64,
    pub phase: String,
    pub parent_rss_bytes: i64,
    pub owned_rss_bytes: i64,
    pub owned_process_count: i64,
    pub owned_memory_bytes: i64,
    pub memory_metric: String,
}

impl ResourceSample {

    pub fn checked(self) -> TelemetryResult<Self> {
        if self.sequence <= 0 {
            return err("sample sequence must be positive");
        }
        if self.elapsed_s.is_nan() || self.elapsed_s < 0.0 {
            return err("sample elapsed time must be non-negative");
        }
        require_generic_phase(&self.phase)?;
        for (name, v) in [
            ("parent_rss_bytes", self.parent_rss_bytes),
            ("owned_rss_bytes", self.owned_rss_bytes),
            ("owned_process_count", self.owned_process_count),
        ] {
            if v < 0 {
                return Err(ResourceTelemetryError(format!("{name} must be non-negative")));
            }
        }
        if self.owned_memory_bytes < self.owned_rss_bytes {
            return err("owned_memory_bytes must be an integer at least owned_rss_bytes");
        }
        if self.memory_metric != "rss" && self.memory_metric != "sum_process_max_rss_physical_footprint" {
            return err("unsupported budgeted memory metric");
        }
        Ok(self)
    }

    #[must_use]
    pub fn to_private_wire(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String("implexity-parent-resource-sample/1".into()));
        m.insert("sequence".into(), Value::from(self.sequence));
        m.insert("elapsed_s".into(), crate::canonical::f(self.elapsed_s));
        m.insert("monotonic_ns".into(), Value::from(self.monotonic_ns));
        m.insert("phase".into(), Value::String(self.phase.clone()));
        m.insert("parent_rss_bytes".into(), Value::from(self.parent_rss_bytes));
        m.insert("owned_rss_bytes".into(), Value::from(self.owned_rss_bytes));
        m.insert("owned_process_count".into(), Value::from(self.owned_process_count));
        m.insert("owned_memory_bytes".into(), Value::from(self.owned_memory_bytes));
        m.insert("memory_metric".into(), Value::String(self.memory_metric.clone()));
        Value::Object(m)
    }


    pub fn from_private_wire(row: &Value) -> TelemetryResult<Self> {
        let m = row
            .as_object()
            .ok_or_else(|| ResourceTelemetryError("resource sample must be an object".into()))?;
        if m.get("schema").and_then(Value::as_str) != Some("implexity-parent-resource-sample/1") {
            return err("unsupported resource sample schema");
        }
        let int = |k: &str| -> TelemetryResult<i64> {
            match m.get(k) {
                Some(Value::Number(n)) if !n.is_f64() => {
                    n.as_i64().ok_or_else(|| ResourceTelemetryError(format!("{k} must be an integer")))
                }
                _ => Err(ResourceTelemetryError(format!("{k} must be an integer"))),
            }
        };
        let elapsed = match m.get("elapsed_s") {
            Some(Value::Number(n)) if n.is_f64() => n.as_f64().unwrap_or(-1.0),
            _ => return err("sample elapsed time must be non-negative"),
        };
        Self {
            sequence: int("sequence")?,
            elapsed_s: elapsed,
            monotonic_ns: int("monotonic_ns")?,
            phase: m.get("phase").and_then(Value::as_str).unwrap_or("").to_string(),
            parent_rss_bytes: int("parent_rss_bytes")?,
            owned_rss_bytes: int("owned_rss_bytes")?,
            owned_process_count: int("owned_process_count")?,
            owned_memory_bytes: int("owned_memory_bytes")?,
            memory_metric: m.get("memory_metric").and_then(Value::as_str).unwrap_or("").to_string(),
        }
        .checked()
    }
}

fn monotonic_ns(origin: Instant) -> i64 {
    i64::try_from(origin.elapsed().as_nanos()).unwrap_or(i64::MAX)
}

#[derive(Debug)]
pub struct ParentResourceTelemetry {
    sink: PathBuf,
    recent: Mutex<(VecDeque<ResourceSample>, i64)>,
    limit: usize,
    started: Instant,
    owner_pid: u32,
    reader: ProcessTableReader,
}

impl ParentResourceTelemetry {

    pub fn new(sink: &Path, recent_limit: usize) -> TelemetryResult<Self> {
        if !sink.is_absolute() || !sink.parent().is_some_and(Path::is_dir) {
            return err("telemetry sink must have an existing absolute parent");
        }
        if recent_limit == 0 {
            return err("recent_limit must be positive");
        }
        Ok(Self {
            sink: sink.to_path_buf(),
            recent: Mutex::new((VecDeque::with_capacity(recent_limit), 0)),
            limit: recent_limit,
            started: Instant::now(),
            owner_pid: std::process::id(),
            reader: ProcessTableReader::new()?,
        })
    }


    pub fn sample(&self, phase: &str, owned: &[ProcessObservation]) -> TelemetryResult<ResourceSample> {
        if std::process::id() != self.owner_pid {
            return err("telemetry writer cannot be reused after fork");
        }
        require_generic_phase(phase)?;
        let darwin = self.reader.is_darwin();
        if darwin && owned.iter().any(|row| row.footprint_bytes.is_none()) {
            return err("Darwin owned memory requires measured physical footprint");
        }
        let parent = self.reader.observe(i64::from(self.owner_pid))?;
        let parent_rss =
            parent.map_or_else(crate::provider_job_authority::peak_memory_bytes, |p| p.rss_bytes);
        let mut guard = self
            .recent
            .lock()
            .map_err(|_| ResourceTelemetryError("resource telemetry state is unavailable".into()))?;
        guard.1 += 1;
        let sample = ResourceSample {
            sequence: guard.1,
            elapsed_s: self.started.elapsed().as_secs_f64(),
            monotonic_ns: monotonic_ns(self.started),
            phase: phase.to_string(),
            parent_rss_bytes: parent_rss.max(0),
            owned_rss_bytes: owned.iter().map(|r| r.rss_bytes).sum(),
            owned_process_count: i64::try_from(owned.len()).unwrap_or(i64::MAX),
            owned_memory_bytes: owned.iter().map(ProcessObservation::budgeted_memory_bytes).sum(),
            memory_metric: if darwin { "sum_process_max_rss_physical_footprint" } else { "rss" }.into(),
        }
        .checked()?;
        let mut encoded = crate::canonical::canonical_text(&sample.to_private_wire());
        encoded.push('\n');
        let append = || -> std::io::Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.append(true).create(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                #[allow(clippy::cast_possible_wrap)]
                options.mode(0o600).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
            }
            let mut file = options.open(&self.sink)?;
            file.write_all(encoded.as_bytes())
        };
        append().map_err(|_| ResourceTelemetryError("resource telemetry append failed".into()))?;
        if guard.0.len() == self.limit {
            guard.0.pop_front();
        }
        guard.0.push_back(sample.clone());
        Ok(sample)
    }

    #[must_use]
    pub fn recent(&self) -> Vec<ResourceSample> {
        self.recent.lock().map(|g| g.0.iter().cloned().collect()).unwrap_or_default()
    }
}

