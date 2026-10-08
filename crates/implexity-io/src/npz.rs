// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::path::Path;

use crate::npy::{NpyArray, NpyError};
use crate::zip::{DEFLATED, STORED, ZipArchive, ZipError, ZipWriter};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NpzError {
    #[error("{0}")]
    Zip(#[from] ZipError),
    #[error("{0}: {1}")]
    Member(String, NpyError),
    #[error("{0}")]
    Io(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Npz {
    members: Vec<(String, NpyArray)>,
}

impl Npz {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: &str, array: NpyArray) {
        if let Some(slot) = self.members.iter_mut().find(|(k, _)| k == key) {
            slot.1 = array;
        } else {
            self.members.push((key.to_string(), array));
        }
    }

    #[must_use]
    pub fn files(&self) -> Vec<&str> {
        self.members.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&NpyArray> {
        self.members.iter().find(|(k, _)| k == key).map(|(_, a)| a)
    }

    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    #[must_use]
    pub fn members(&self) -> &[(String, NpyArray)] {
        &self.members
    }

    #[must_use]
    pub fn into_map(self) -> BTreeMap<String, NpyArray> {
        self.members.into_iter().collect()
    }
}


pub fn load(raw: &[u8]) -> Result<Npz, NpzError> {
    let z = ZipArchive::new(raw)?;
    let mut out = Npz::new();
    for e in z.entries() {
        let data = z.read_entry(e)?;
        let key = e.name.strip_suffix(".npy").unwrap_or(&e.name).to_string();
        let array = NpyArray::from_bytes(&data).map_err(|x| NpzError::Member(e.name.clone(), x))?;
        out.insert(&key, array);
    }
    Ok(out)
}


pub fn load_file(path: &Path) -> Result<Npz, NpzError> {
    let raw = std::fs::read(path).map_err(|e| NpzError::Io(format!("{}: {e}", path.display())))?;
    load(&raw)
}

fn write(members: &[(&str, &NpyArray)], method: u16, level: u32) -> Result<Vec<u8>, NpzError> {
    let mut w = ZipWriter::new();
    for (key, array) in members {
        let name = format!("{key}.npy");
        let bytes = array.to_bytes().map_err(|x| NpzError::Member(name.clone(), x))?;
        w.add(&name, &bytes, method, level)?;
    }
    Ok(w.finish()?)
}


pub fn save(members: &[(&str, &NpyArray)]) -> Result<Vec<u8>, NpzError> {
    write(members, STORED, 0)
}


pub fn save_compressed(members: &[(&str, &NpyArray)]) -> Result<Vec<u8>, NpzError> {
    write(members, DEFLATED, 6)
}


pub fn save_deterministic(arrays: &BTreeMap<String, NpyArray>) -> Result<Vec<u8>, NpzError> {
    let members: Vec<(&str, &NpyArray)> = arrays.iter().map(|(k, v)| (k.as_str(), v)).collect();
    write(&members, DEFLATED, 9)
}


pub fn save_f64_streamed<W: std::io::Write + std::io::Seek>(output: W, arrays: &BTreeMap<String, ndarray::ArrayD<f64>>) -> Result<W, NpzError> {
    let mut zip=crate::zip::StreamZipWriter::new(output);
    for (name,array) in arrays {
        let header=crate::npy::header_bytes("<f8",false,array.shape()).map_err(|e|NpzError::Member(name.clone(),e))?;
        let size=(array.len() as u64).checked_mul(8).and_then(|n|n.checked_add(header.len() as u64)).ok_or_else(||NpzError::Io("NPY byte length overflow".into()))?;
        zip.add(&format!("{name}.npy"),size,|sink| {
            sink.write_all(&header)?;
            let mut chunk=Vec::with_capacity(65536);
            for value in array {
                chunk.extend_from_slice(&value.to_le_bytes());
                if chunk.len()==65536 {sink.write_all(&chunk)?;chunk.clear();}
            }
            sink.write_all(&chunk)
        })?;
    }
    Ok(zip.finish()?)
}
