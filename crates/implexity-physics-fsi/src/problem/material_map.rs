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

use serde_json::{Value, json};

use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::soft::laws::Material;
use implexity_physics_solid::soft_fsi::voxel::VoxelGrid;

use crate::json::{Section, refuse, strip_nulls};
use crate::problem::design::selection;

pub const ENTRY_KEYS: [&str; 5] = ["label", "region", "material", "scale", "fibre_directions"];
pub const SCALE_KEYS: [&str; 3] = ["stiffness", "density", "prony"];
pub const MAX_ENTRIES: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub struct MaterialMap {
    pub materials: Vec<Material>,
    pub voxel_material: Vec<usize>,
    pub labels: Vec<String>,
    pub law_objects: Vec<Value>,
    pub fibre_directions: Vec<Option<Vec<[f64; 3]>>>,
}

impl MaterialMap {
    #[must_use]
    pub fn uniform(base: Material, voxels: usize) -> Self {
        Self {
            materials: vec![base],
            voxel_material: vec![0; voxels],
            labels: Vec::new(),
            law_objects: Vec::new(),
            fibre_directions: vec![None],
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
                            Value::Number(n) => {
                                n.as_f64().map_or_else(|| x.clone(), |f| json!(f * factor))
                            }
                            Value::Array(a) => Value::Array(
                                a.iter()
                                    .map(|e| {
                                        e.as_f64().map_or_else(|| e.clone(), |f| json!(f * factor))
                                    })
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

pub fn scaled_law(
    base: &Value,
    stiffness: f64,
    density: f64,
    prony: Option<&Value>,
) -> CaeResult<Value> {
    if !(stiffness.is_finite() && stiffness > 0.0 && density.is_finite() && density > 0.0) {
        return refuse("material scale factors must be finite and positive");
    }
    let Value::Object(_) = base else {
        return refuse("the base material must be a law object");
    };
    let mut out = scale_moduli(base, stiffness);
    if let Some(m) = out.as_object_mut() {
        let rho = base
            .get("density_kg_m3")
            .and_then(Value::as_f64)
            .unwrap_or(1000.0);
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
        Some(_) => {
            return refuse("solid.material_map must be a list of {label, region, material | scale}");
        }
    };
    if rows.len() > MAX_ENTRIES {
        return refuse(format!(
            "solid.material_map holds at most {MAX_ENTRIES} entries"
        ));
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
            s.raw("region")
                .ok_or_else(|| CaeError::contract(format!("{path}.region is required")))?,
            &s.at("region"),
            grid,
        )?;
        if !flags.iter().any(|f| *f) {
            return refuse(format!("{path}.region selects no voxel"));
        }
        s.put("region", region);
        let law = match (s.raw("material"), s.raw("scale")) {
            (Some(_), Some(_)) => {
                return refuse(format!("{path}: give either material or scale, not both"));
            }
            (None, None) => {
                return refuse(format!(
                    "{path} needs a material law object or a scale of the base"
                ));
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
        let directions = match s.raw("fibre_directions") {
            None | Some(Value::Null) => None,
            Some(v) => {
                if material.fibres.is_none() {
                    return refuse(format!("{path}.fibre_directions requires fibres"));
                }
                let rows = v
                    .as_array()
                    .filter(|v| (1..=4).contains(&v.len()))
                    .ok_or_else(|| {
                        CaeError::contract(format!(
                            "{path}.fibre_directions must list 1..4 XYZ directions"
                        ))
                    })?;
                let mut dirs = Vec::new();
                for row in rows {
                    let a = row.as_array().filter(|a| a.len() == 3).ok_or_else(|| {
                        CaeError::contract(format!("{path}.fibre_directions requires XYZ triples"))
                    })?;
                    let mut d = [0.0; 3];
                    for i in 0..3 {
                        d[i] = a[i].as_f64().filter(|v| v.is_finite()).ok_or_else(|| {
                            CaeError::contract(format!("{path}.fibre_directions must be finite"))
                        })?;
                    }
                    let norm = d.iter().map(|v| v * v).sum::<f64>().sqrt();
                    if !norm.is_finite() || norm <= 0.0 {
                        return refuse(format!("{path}.fibre_directions must be nonzero"));
                    }
                    for x in &mut d {
                        *x /= norm;
                    }
                    dirs.push(d);
                }
                if let Some(p) = &material.directional_prony
                    && p.fibres.len() != dirs.len()
                {
                    return refuse(format!(
                        "{path}.directional_prony requires one spectrum per fibre direction"
                    ));
                }
                s.put("fibre_directions", json!(dirs));
                Some(dirs)
            }
        };
        let index = map.materials.len();
        for (voxel, flag) in flags.iter().enumerate() {
            if *flag {
                map.voxel_material[voxel] = index;
            }
        }
        map.fibre_directions.push(directions);
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
