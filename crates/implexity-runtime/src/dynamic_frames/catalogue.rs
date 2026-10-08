// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::store::{DynamicStore, MANIFEST_FILE};
use super::{DynamicError, DynamicResult};

pub const JOB_ROOT: &str = "dynamic";
pub const SERVICE_ROOT: &str = "dynamic_results";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalogue {
    pub service_root: Option<PathBuf>,
    pub jobs_root: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreId {
    Service(String),
    Job(String, String),
}

fn is_store_name(s: &str) -> bool {
    (1..=96).contains(&s.len())
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

#[must_use]
pub fn is_job_id(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl StoreId {


    pub fn parse(s: &str) -> DynamicResult<Self> {
        let parts: Vec<&str> = s.split('/').collect();
        match parts.as_slice() {
            ["service", name] if is_store_name(name) => Ok(Self::Service((*name).to_owned())),
            ["job", job, name] if is_job_id(job) && is_store_name(name) => {
                Ok(Self::Job((*job).to_owned(), (*name).to_owned()))
            }
            _ => Err(DynamicError::invalid(
                "a dynamic result store is named service/<store> or job/<12-hex job id>/<store>",
            )),
        }
    }

    #[must_use]
    pub fn as_string(&self) -> String {
        match self {
            Self::Service(n) => format!("service/{n}"),
            Self::Job(j, n) => format!("job/{j}/{n}"),
        }
    }

    #[must_use]
    pub fn job(&self) -> Option<&str> {
        match self {
            Self::Job(j, _) => Some(j),
            Self::Service(_) => None,
        }
    }
}

fn store_dirs(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter(|e| std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(n, p)| is_store_name(n) && p.join(MANIFEST_FILE).is_file())
        .collect();
    out.sort();
    out
}

impl Catalogue {


    pub fn resolve(&self, id: &StoreId) -> DynamicResult<PathBuf> {
        let dir = match id {
            StoreId::Service(n) => self.service_root.as_ref().map(|r| r.join(n)),
            StoreId::Job(j, n) => self.jobs_root.as_ref().map(|r| r.join(j).join(JOB_ROOT).join(n)),
        }
        .ok_or_else(|| DynamicError::invalid("dynamic results are not available in this service"))?;
        let meta = std::fs::symlink_metadata(&dir);
        if !meta.is_ok_and(|m| m.is_dir()) || !dir.join(MANIFEST_FILE).is_file() {
            return Err(DynamicError::invalid(format!("no dynamic result store {}", id.as_string())));
        }
        Ok(dir)
    }



    pub fn open(&self, id: &StoreId) -> DynamicResult<DynamicStore> {
        DynamicStore::open(&self.resolve(id)?)
    }

    #[must_use]
    pub fn ids(&self, job: Option<&str>) -> Vec<StoreId> {
        let mut out = Vec::new();
        if job.is_none()
            && let Some(root) = &self.service_root
        {
            out.extend(store_dirs(root).into_iter().map(|(n, _)| StoreId::Service(n)));
        }
        if let Some(root) = &self.jobs_root {
            let mut jobs: Vec<String> = std::fs::read_dir(root)
                .map(|rd| {
                    rd.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect()
                })
                .unwrap_or_default();
            jobs.retain(|j| is_job_id(j) && job.is_none_or(|x| x == j));
            jobs.sort();
            for j in jobs {
                out.extend(
                    store_dirs(&root.join(&j).join(JOB_ROOT))
                        .into_iter()
                        .map(|(n, _)| StoreId::Job(j.clone(), n)),
                );
            }
        }
        out
    }

    #[must_use]
    pub fn list(&self, job: Option<&str>, limit: usize) -> Value {
        let ids = self.ids(job);
        let skip = ids.len().saturating_sub(limit);
        let rows: Vec<Value> = ids[skip..]
            .iter()
            .map(|id| match self.open(id) {
                Ok(s) => {
                    let m = s.manifest();
                    json!({
                        "store": id.as_string(), "job_id": id.job(),
                        "state": s.status().get("state").cloned().unwrap_or(json!("unknown")),
                        "provenance": m.provenance,
                        "fields": m.fields.iter().map(|f| json!({"name": f.name, "label": f.label, "unit": f.unit,
                            "kind": f.kind.as_str(), "grid": f.grid, "palette": f.palette.as_str()})).collect::<Vec<_>>(),
                        "series": m.series.iter().map(|x| json!({"name": x.name, "label": x.label, "unit": x.unit, "role": x.role})).collect::<Vec<_>>(),
                        "grids": m.grids.iter().map(|g| json!({"name": g.name, "shape": g.shape, "unit": g.unit})).collect::<Vec<_>>(),
                        "frames": s.frames().len(), "period": s.period(), "time_unit": m.time.unit,
                        "t_range": [s.frames().first().map(|f| f.t), s.frames().last().map(|f| f.t)],
                        "bytes": s.status().get("bytes").cloned().unwrap_or(Value::Null),
                    })
                }
                Err(e) => json!({"store": id.as_string(), "job_id": id.job(), "state": "unreadable", "error": e.to_string()}),
            })
            .collect();
        json!({"schema": "implexity-dynamic-results/1", "extension": super::EXTENSION,
               "count": ids.len(), "listed": rows.len(), "stores": rows})
    }
}

