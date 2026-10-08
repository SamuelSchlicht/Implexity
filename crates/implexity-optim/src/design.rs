// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::Path;

use implexity_core::contracts::TOPOLOGY_COORDINATE;
use implexity_core::numeric_contract::real_array;
use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::numeric::{array_to_value, shape_repr, str_list_repr};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NamedArrays {
    entries: Vec<(String, ArrayD<f64>)>,
}

impl NamedArrays {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_pairs<I: IntoIterator<Item = (String, ArrayD<f64>)>>(pairs: I) -> Self {
        let mut out = Self::new();
        for (k, v) in pairs {
            out.insert(k, v);
        }
        out
    }

    #[must_use]
    pub fn single(name: &str, value: ArrayD<f64>) -> Self {
        Self { entries: vec![(name.to_string(), value)] }
    }

    pub fn insert(&mut self, name: impl Into<String>, value: ArrayD<f64>) {
        let name = name.into();
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == name) {
            slot.1 = value;
        } else {
            self.entries.push((name, value));
        }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ArrayD<f64>> {
        self.entries.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut ArrayD<f64>> {
        self.entries.iter_mut().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == name)
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ArrayD<f64>)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut ArrayD<f64>)> {
        self.entries.iter_mut().map(|(k, v)| (k.as_str(), v))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn first(&self) -> Option<(&str, &ArrayD<f64>)> {
        self.entries.first().map(|(k, v)| (k.as_str(), v))
    }

    #[must_use]
    pub fn same_names(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self.entries.iter().zip(&other.entries).all(|((a, _), (b, _))| a == b)
    }

    #[must_use]
    pub fn same_name_set(&self, names: &[String]) -> bool {
        self.entries.len() == names.len() && names.iter().all(|n| self.contains(n))
    }

    #[must_use]
    pub fn arrays_equal(&self, other: &Self) -> bool {
        self.iter().all(|(k, v)| other.get(k).is_some_and(|w| w.shape() == v.shape() && w == v))
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        Value::Object(self.iter().map(|(k, v)| (k.to_string(), array_to_value(v))).collect())
    }


    pub fn from_wire(value: &Value, label: &str) -> CaeResult<Self> {
        let Some(map) = value.as_object() else {
            return Err(CaeError::contract(format!("{label} must be a mapping of arrays")));
        };
        let mut out = Self::new();
        for (k, v) in map {
            out.insert(k.clone(), real_array(v, &format!("{label} {}", repr_str(k)))?);
        }
        Ok(out)
    }


    pub fn owned_design(&self) -> CaeResult<Self> {
        if self.is_empty() {
            return Err(CaeError::contract("named design must be a nonempty mapping"));
        }
        if self.entries.iter().any(|(k, _)| k.is_empty()) {
            return Err(CaeError::contract("named-design coordinate ids must be nonempty text"));
        }
        for (k, v) in self.iter() {
            require_finite(v, &format!("design coordinate {}", repr_str(k)))?;
        }
        Ok(self.clone())
    }
}

impl FromIterator<(String, ArrayD<f64>)> for NamedArrays {
    fn from_iter<T: IntoIterator<Item = (String, ArrayD<f64>)>>(iter: T) -> Self {
        Self::from_pairs(iter)
    }
}


pub fn require_finite(a: &ArrayD<f64>, label: &str) -> CaeResult<()> {
    if a.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(CaeError::contract(format!("{label} must contain only finite values")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignLayout {
    pub names: Vec<String>,
    pub shapes: Vec<Vec<usize>>,
    pub sizes: Vec<usize>,
}

impl DesignLayout {

    pub fn from_values(values: &NamedArrays) -> CaeResult<Self> {
        if values.is_empty() {
            return Err(CaeError::contract("named design requires a nonempty mapping"));
        }
        let names = values.names();
        if names.iter().any(String::is_empty) {
            return Err(CaeError::contract("design coordinate names must be nonempty strings"));
        }
        let mut shapes = Vec::with_capacity(names.len());
        let mut sizes = Vec::with_capacity(names.len());
        for (name, a) in values.iter() {
            require_finite(a, &format!("{name}: design"))?;
            if a.ndim() == 0 || a.is_empty() {
                return Err(CaeError::contract(format!(
                    "{name}: design must be a finite nonempty nonscalar array"
                )));
            }
            shapes.push(a.shape().to_vec());
            sizes.push(a.len());
        }
        Ok(Self { names, shapes, sizes })
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.sizes.iter().sum()
    }

    #[must_use]
    pub fn slices(&self) -> Vec<(String, usize, usize)> {
        let mut offset = 0;
        self.names
            .iter()
            .zip(&self.sizes)
            .map(|(n, s)| {
                let row = (n.clone(), offset, offset + s);
                offset += s;
                row
            })
            .collect()
    }


    pub fn check_keys(&self, values: &NamedArrays, label: &str) -> CaeResult<()> {
        let missing: Vec<&String> = self.names.iter().filter(|n| !values.contains(n)).collect();
        let extra: Vec<String> = values.names().into_iter().filter(|n| !self.names.contains(n)).collect();
        if missing.is_empty() && extra.is_empty() {
            return Ok(());
        }
        Err(CaeError::contract(format!(
            "{label}: coordinate mismatch; missing={}, extra={}",
            str_list_repr(&missing),
            str_list_repr(&extra)
        )))
    }


    pub fn pack(&self, values: &NamedArrays, label: &str) -> CaeResult<Vec<f64>> {
        self.check_keys(values, label)?;
        let mut out = Vec::with_capacity(self.size());
        for (name, shape) in self.names.iter().zip(&self.shapes) {
            let a = values.get(name).ok_or_else(|| CaeError::contract(format!("{label}/{name}: missing")))?;
            require_finite(a, &format!("{label}/{name}"))?;
            if a.shape() != shape.as_slice() {
                return Err(CaeError::contract(format!(
                    "{label}/{name}: expected finite shape {}, got {}",
                    shape_repr(shape),
                    shape_repr(a.shape())
                )));
            }
            out.extend(a.iter().copied());
        }
        Ok(out)
    }


    pub fn unpack(&self, flat: &[f64]) -> CaeResult<NamedArrays> {
        if flat.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("packed design/gradient must contain only finite values"));
        }
        if flat.len() != self.size() {
            return Err(CaeError::contract(format!(
                "packed design/gradient must have finite shape ({},)",
                self.size()
            )));
        }
        let mut out = NamedArrays::new();
        let mut offset = 0;
        for ((name, shape), size) in self.names.iter().zip(&self.shapes).zip(&self.sizes) {
            let part = flat[offset..offset + size].to_vec();
            offset += size;
            let a = ArrayD::from_shape_vec(IxDyn(shape), part)
                .map_err(|e| CaeError::contract(format!("{name}: {e}")))?;
            out.insert(name.clone(), a);
        }
        Ok(out)
    }


    pub fn repack(&self, values: &NamedArrays, label: &str) -> CaeResult<NamedArrays> {
        self.unpack(&self.pack(values, label)?)
    }
}


pub fn design_identity(design: &NamedArrays) -> CaeResult<String> {
    let layout = DesignLayout::from_values(design)?;
    let mut rows = Vec::with_capacity(layout.names.len());
    for (name, shape) in layout.names.iter().zip(&layout.shapes) {
        let a = design.get(name).ok_or_else(|| CaeError::contract("design identity lost a coordinate"))?;
        let mut hasher = Sha256::new();
        for v in a {
            hasher.update(v.to_le_bytes());
        }
        rows.push(json!({
            "coordinate": name,
            "shape": shape,
            "dtype": "<f8",
            "sha256": hex::encode(hasher.finalize()),
        }));
    }
    let payload = implexity_core::json::canonical(&Value::Array(rows));
    Ok(format!("design-{}", implexity_core::json::sha256_hex(payload.as_bytes())))
}


pub fn load_design(path: &Path) -> CaeResult<NamedArrays> {
    let npz = implexity_io::npz::load_file(path)
        .map_err(|e| CaeError::contract(format!("native design snapshot unreadable: {e}")))?;
    let strings = |key: &str| -> Option<Vec<String>> { npz.get(key).and_then(npy_strings) };
    let (Some(refs), Some(slots)) = (strings("refs"), strings("slots")) else {
        return Err(CaeError::contract("native design snapshot requires refs and slots"));
    };
    let unique = |v: &[String]| {
        let mut s = v.to_vec();
        s.sort();
        s.dedup();
        s.len() == v.len()
    };
    if refs.len() != slots.len() || !unique(&refs) || !unique(&slots) {
        return Err(CaeError::contract("native snapshot coordinate/slot names are duplicate or mismatched"));
    }
    let mut design = NamedArrays::new();
    for (r, s) in refs.iter().zip(&slots) {
        let a = npz
            .get(&format!("p_{s}"))
            .and_then(implexity_io::npy::NpyArray::to_f64)
            .ok_or_else(|| CaeError::contract("native snapshot omitted a declared design array"))?;
        design.insert(r.clone(), a);
    }
    let layout = DesignLayout::from_values(&design)?;
    if !layout.names.iter().any(|n| n == TOPOLOGY_COORDINATE) {
        return Err(CaeError::contract(format!("named design snapshot omits {TOPOLOGY_COORDINATE}")));
    }
    Ok(design)
}

fn npy_strings(a: &implexity_io::npy::NpyArray) -> Option<Vec<String>> {
    use implexity_io::npy::NpyData;
    match &a.data {
        NpyData::Unicode { values, .. } => Some(values.clone()),
        NpyData::Bytes { values, .. } => {
            Some(values.iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect())
        }
        _ => None,
    }
}

#[must_use]
pub fn names_value(names: &[String]) -> Value {
    Value::Array(names.iter().cloned().map(Value::String).collect())
}

#[must_use]
pub fn empty_object() -> Value {
    Value::Object(Map::new())
}

