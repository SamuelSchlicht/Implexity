// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use implexity_core::{CaeError,CaeResult};
use crate::matrix::Jacobian;
use crate::trace::Fields;

static NEXT: AtomicU64=AtomicU64::new(1);

#[derive(Clone,Debug)]
pub struct RejectedStateCapture {
    pub directory: PathBuf,
    pub source_sha256: String,
    pub provenance: Value,
}

impl RejectedStateCapture {
    pub fn validate(&self) -> CaeResult<()> {
        if !self.directory.is_absolute() || !self.directory.is_dir() {
            return Err(CaeError::contract("rejected-state capture requires an existing absolute directory"));
        }
        if !crate::operation_context::is_sha256_hex(&self.source_sha256) {
            return Err(CaeError::contract("rejected-state capture requires a source SHA-256 digest"));
        }
        Ok(())
    }

    pub fn record(&self,state:&[f64],design:&[f64],context:Option<&[f64]>,residual:&[f64],matrix:&Jacobian,fields:&Fields,error:&CaeError) -> CaeResult<PathBuf> {
        self.validate()?;
        let sparse=matrix.to_csr()?;
        let path=self.directory.join(format!("rejected-{}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));
        fs::create_dir(&path).map_err(io)?;
        let mut arrays=serde_json::Map::new();
        for (name,values) in [("state",state),("design",design),("residual",residual),("jacobian_values",sparse.data())] {
            arrays.insert(name.into(),write_f64(&path,name,values)?);
        }
        if let Some(previous)=context { arrays.insert("context".into(),write_f64(&path,"context",previous)?); }
        arrays.insert("jacobian_indptr".into(),write_u64(&path,"jacobian_indptr",sparse.indptr())?);
        arrays.insert("jacobian_indices".into(),write_u64(&path,"jacobian_indices",sparse.indices())?);
        let record=json!({"schema":"implexity-rejected-state/1","source_sha256":self.source_sha256,"caller_provenance":self.provenance,"fields":fields,"original_error":error.to_string(),"jacobian":{"format":"CSR","rows":sparse.nrows(),"columns":sparse.ncols(),"nnz":sparse.nnz(),"scope":"original_full_normalized_jacobian"},"arrays":arrays,"numerical_admission":false,"pid":std::process::id()});
        fs::write(path.join("receipt.json"),serde_json::to_vec_pretty(&record).map_err(|e|CaeError::contract(e.to_string()))?).map_err(io)?;
        Ok(path)
    }
}

fn io(error:std::io::Error)->CaeError { CaeError::contract(format!("rejected-state capture: {error}")) }

fn write_f64(path:&Path,name:&str,values:&[f64])->CaeResult<Value> {
    let filename=format!("{name}.f64le");
    let mut file=File::create(path.join(&filename)).map_err(io)?;
    let mut hash=Sha256::new();
    for v in values { let b=v.to_le_bytes(); file.write_all(&b).map_err(io)?; hash.update(b); }
    file.sync_all().map_err(io)?;
    Ok(json!({"file":filename,"length":values.len(),"dtype":"f64le","sha256":hex::encode(hash.finalize())}))
}

fn write_u64(path:&Path,name:&str,values:&[usize])->CaeResult<Value> {
    let filename=format!("{name}.u64le");
    let mut file=File::create(path.join(&filename)).map_err(io)?;
    let mut hash=Sha256::new();
    for &v in values { let b=u64::try_from(v).map_err(|_|CaeError::contract("capture index overflow"))?.to_le_bytes(); file.write_all(&b).map_err(io)?;hash.update(b); }
    file.sync_all().map_err(io)?;
    Ok(json!({"file":filename,"length":values.len(),"dtype":"u64le","sha256":hex::encode(hash.finalize())}))
}
