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
