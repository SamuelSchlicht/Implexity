// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::soft::laws::Material;
use implexity_physics_solid::soft_fsi::voxel::VoxelGrid;

use crate::json::{Section, refuse, strip_nulls};
use crate::problem::design::selection;

pub const ENTRY_KEYS: [&str; 4] = ["label", "region", "material", "scale"];
pub const SCALE_KEYS: [&str; 3] = ["stiffness", "density", "prony"];
pub const MAX_ENTRIES: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub struct MaterialMap {
    pub materials: Vec<Material>,
    pub voxel_material: Vec<usize>,
    pub labels: Vec<String>,
    pub law_objects: Vec<Value>,
}

impl MaterialMap {
    #[must_use]
    pub fn uniform(base: Material, voxels: usize) -> Self {
        Self {
            materials: vec![base],
            voxel_material: vec![0; voxels],
            labels: Vec::new(),
            law_objects: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_uniform(&self) -> bool {
        self.materials.len() == 1
    }

    #[must_use]
    pub fn voxels_of(&self, k: usize) -> Vec<bool> {
        self.voxel_material.iter().map(|m| *m == k).collect()
    }
}

fn scale_moduli(v: &Value, factor: f64) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| {
                    let scaled = if k.ends_with("_Pa") {
                        match x {
                            Value::Number(n) => n.as_f64().map_or_else(|| x.clone(), |f| json!(f * factor)),
                            Value::Array(a) => Value::Array(
                                a.iter()
                                    .map(|e| e.as_f64().map_or_else(|| e.clone(), |f| json!(f * factor)))
                                    .collect(),
                            ),
                            other => other.clone(),
                        }
                    } else {
                        scale_moduli(x, factor)
                    };
                    (k.clone(), scaled)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}


pub fn scaled_law(base: &Value, stiffness: f64, density: f64, prony: Option<&Value>) -> CaeResult<Value> {
    if !(stiffness.is_finite() && stiffness > 0.0 && density.is_finite() && density > 0.0) {
        return refuse("material scale factors must be finite and positive");
    }
    let Value::Object(_) = base else {
        return refuse("the base material must be a law object");
    };
    let mut out = scale_moduli(base, stiffness);
    if let Some(m) = out.as_object_mut() {
        let rho = base.get("density_kg_m3").and_then(Value::as_f64).unwrap_or(1000.0);
        m.insert("density_kg_m3".into(), json!(rho * density));
        match prony {
            None => {}
            Some(Value::Null) => {
                m.remove("prony");
            }
            Some(p) => {
                m.insert("prony".into(), p.clone());
            }
        }
    }
    Ok(out)
}


pub fn parse(
    v: Option<&Value>,
    base_law: &Value,
    base: &Material,
    grid: &VoxelGrid,
) -> CaeResult<(MaterialMap, Value)> {
    let n = grid.voxel_count();
    let mut map = MaterialMap::uniform(base.clone(), n);
    let rows = match v {
        None | Some(Value::Null) => return Ok((map, json!([]))),
        Some(Value::Array(a)) => a,
        Some(_) => return refuse("solid.material_map must be a list of {label, region, material | scale}"),
    };
    if rows.len() > MAX_ENTRIES {
        return refuse(format!("solid.material_map holds at most {MAX_ENTRIES} entries"));
    }
    let mut normal = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let path = format!("solid.material_map[{i}]");
        let mut s = Section::new(row, &path, &ENTRY_KEYS)?;
        let label = s.text_or("label", &format!("material{}", i + 1), 64)?;
        if map.labels.contains(&label) {
            return refuse(format!("{path}.label {label:?} is repeated"));
        }
        let (flags, region) = selection(
            s.raw("region").ok_or_else(|| CaeError::contract(format!("{path}.region is required")))?,
            &s.at("region"),
            grid,
        )?;
        if !flags.iter().any(|f| *f) {
            return refuse(format!("{path}.region selects no voxel"));
        }
        s.put("region", region);
        let law = match (s.raw("material"), s.raw("scale")) {
            (Some(_), Some(_)) => return refuse(format!("{path}: give either material or scale, not both")),
            (None, None) => {
                return refuse(format!("{path} needs a material law object or a scale of the base"));
            }
            (Some(m), None) => {
                s.put("scale", Value::Null);
                strip_nulls(m)
            }
            (None, Some(_)) => {
                let mut c = s.child("scale", &SCALE_KEYS)?;
                let stiffness = c.number_or("stiffness", 1.0, |x| x > 0.0, "positive")?;
                let density = c.number_or("density", 1.0, |x| x > 0.0, "positive")?;
                let prony = match c.raw("prony") {
                    None => {
                        c.put("prony", json!("base"));
                        None
                    }
                    Some(Value::String(t)) if t == "base" => {
                        c.put("prony", json!("base"));
                        None
                    }
                    Some(p) => {
                        c.put("prony", p.clone());
                        Some(p.clone())
                    }
                };
                s.put("scale", c.finish());
                let null = Value::Null;
                let prony = match &prony {
                    Some(Value::Object(m)) if m.is_empty() => Some(&null),
                    other => other.as_ref(),
                };
                scaled_law(base_law, stiffness, density, prony)?
            }
        };
        let material = implexity_physics_solid::soft::problem::material(&law)
            .map_err(|e| e.context(&format!("{path} material")))?;
        if let Some(Value::Object(_)) = s.raw("material") {
            s.put("material", law.clone());
        } else {
            s.put("material", Value::Null);
        }
        let index = map.materials.len();
        for (voxel, flag) in flags.iter().enumerate() {
            if *flag {
                map.voxel_material[voxel] = index;
            }
        }
        map.materials.push(material);
        map.labels.push(label);
        map.law_objects.push(law);
        normal.push(s.finish());
    }
    for k in 1..map.materials.len() {
        if !map.voxel_material.contains(&k) {
            return refuse(format!(
                "solid.material_map entry {:?} is completely covered by later entries",
                map.labels[k - 1]
            ));
        }
    }
    Ok((map, Value::Array(normal)))
}
