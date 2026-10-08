// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_core::CaeResult;
use implexity_physics_solid::soft::design::{Interpolation, MassInterpolation, Stiffness};
use implexity_physics_solid::soft_fsi::pushforward::BlockingMap;
use implexity_physics_solid::soft_fsi::voxel::VoxelGrid;

use crate::json::{Section, box_of, encode_mask, in_box, mask_of, refuse, values_of};
use crate::problem::removal::{self, RemovalSpec};

pub const DESIGN_KEYS: [&str; 12] = [
    "coordinate",
    "region",
    "removal",
    "protected_density",
    "protected_solid",
    "initial_density",
    "filter_radius_m",
    "projection",
    "interpolation",
    "fluid_blocking",
    "symmetry",
    "two_material",
];

pub const COORDINATE: &str = "model:control";

#[derive(Clone, Debug)]
pub struct DesignSpec {
    pub region: Vec<bool>,
    pub protected_density: Vec<f64>,
    pub initial_density: Vec<f64>,
    pub filter_radius_m: f64,
    pub projection: Option<(f64, f64)>,
    pub interpolation: Interpolation,
    pub blocking: BlockingMap,
    pub constant_blocking: bool,
    pub mirror_axis: Option<usize>,
    pub removal: Option<RemovalSpec>,
}

fn voxel_centroid(grid: &VoxelGrid, v: usize) -> [f64; 3] {
    let s = grid.shape;
    let ijk = [v / (s[1] * s[2]), (v / s[2]) % s[1], v % s[2]];
    std::array::from_fn(|a| grid.origin_m[a] + (ijk[a] as f64 + 0.5) * grid.element_size_m)
}

pub(crate) fn selection(v: &Value, path: &str, grid: &VoxelGrid) -> CaeResult<(Vec<bool>, Value)> {
    let n = grid.voxel_count();
    match v {
        Value::String(s) if s == "all" => Ok((vec![true; n], json!("all"))),
        Value::Array(a) if a.len() == n && a.iter().all(Value::is_boolean) => {
            Ok((a.iter().map(|b| b.as_bool() == Some(true)).collect(), v.clone()))
        }
        Value::Object(m) if m.contains_key("runs") => {
            let mask = mask_of(v, path, grid.shape)?;
            let normal = encode_mask(&mask, grid.shape);
            Ok((mask, normal))
        }
        Value::Object(_) => {
            let s = Section::new(v, path, &["box_m", "boxes_m"])?;
            let mut boxes = Vec::new();
            if let Some(b) = s.raw("box_m") {
                boxes.push(box_of(b, &s.at("box_m"))?);
            }
            if let Some(list) = s.raw("boxes_m") {
                let a = list.as_array().ok_or_else(|| {
                    implexity_core::CaeError::contract(format!("{path}.boxes_m must list boxes"))
                })?;
                for (i, b) in a.iter().enumerate() {
                    boxes.push(box_of(b, &format!("{path}.boxes_m[{i}]"))?);
                }
            }
            if boxes.is_empty() {
                return refuse(format!("{path} needs box_m or boxes_m"));
            }
            let flags: Vec<bool> = (0..n)
                .map(|i| boxes.iter().any(|b| in_box(b, voxel_centroid(grid, i), grid.element_size_m)))
                .collect();
            Ok((flags, json!({"boxes_m": boxes.iter().map(|b| json!([b[0], b[1]])).collect::<Vec<_>>()})))
        }
        _ => refuse(format!(
            "{path} must be \"all\", one Boolean per voxel, a run-length mask {{shape, runs}}, {{box_m}} or {{boxes_m}}"
        )),
    }
}

fn interpolation(s: &mut Section<'_>) -> CaeResult<Interpolation> {
    let mut c = s.child_or_empty(
        "interpolation",
        &["stiffness", "penalty", "q", "e_min", "wang", "wang_beta", "wang_eta", "mass"],
    )?;
    let e_min = c.number_or("e_min", 1e-6, |x| x > 0.0 && x < 1.0, "in (0, 1)")?;
    let stiffness = match c.choice_or("stiffness", "simp", &["simp", "ramp"])?.as_str() {
        "simp" => {
            Stiffness::Simp { penalty: c.number_or("penalty", 3.0, |x| x >= 1.0, "at least 1")?, e_min }
        }
        _ => Stiffness::Ramp { q: c.number_or("q", 8.0, |x| x >= 0.0, "nonnegative")?, e_min },
    };
    let wang = if c.boolean_or("wang", true)? {
        Some((
            c.number_or("wang_beta", 500.0, |x| x > 0.0, "positive")?,
            c.number_or("wang_eta", 0.01, |x| x > 0.0 && x < 1.0, "in (0, 1)")?,
        ))
    } else {
        None
    };
    let mass = match c.choice_or("mass", "pedersen", &["pedersen", "linear", "constant"])?.as_str() {
        "pedersen" => MassInterpolation::Pedersen,
        "constant" => MassInterpolation::Constant,
        _ => MassInterpolation::Linear,
    };
    s.put("interpolation", c.finish());
    let out = Interpolation { stiffness, wang, mass };
    out.validate()?;
    Ok(out)
}


#[allow(clippy::too_many_lines)]
pub fn parse(v: &Value, grid: &VoxelGrid) -> CaeResult<(DesignSpec, Value)> {
    let mut s = Section::new(v, "design", &DESIGN_KEYS)?;
    let coordinate = s.text_or("coordinate", COORDINATE, 64)?;
    if coordinate != COORDINATE {
        return refuse(format!("design.coordinate must be {COORDINATE:?}"));
    }
    let n = grid.voxel_count();
    let (region, rn) = match s.raw("region") {
        None => (vec![true; n], json!("all")),
        Some(r) => selection(r, &s.at("region"), grid)?,
    };
    s.put("region", rn);
    if !region.iter().any(|b| *b) {
        return refuse("design.region selects no voxel");
    }
    let mut protected_density = match s.raw("protected_density") {
        None => {
            s.put("protected_density", json!(if s.has("protected_solid") { 0.0 } else { 1.0 }));
            vec![if s.has("protected_solid") { 0.0 } else { 1.0 }; n]
        }
        Some(Value::Number(_)) => {
            let x = s.number("protected_density", |x| (0.0..=1.0).contains(&x), "in [0, 1]")?;
            vec![x; n]
        }
        Some(other) => {
            let list = match other {
                Value::Object(_) => values_of(other, &s.at("protected_density"), grid.shape, 0.0)?,
                _ => s.numbers_of("protected_density", other, Some(n))?,
            };
            if list.iter().any(|x| !(0.0..=1.0).contains(x)) {
                return refuse("design.protected_density values must lie in [0, 1]");
            }
            s.put("protected_density", json!(list));
            list
        }
    };
    match s.raw("protected_solid") {
        None | Some(Value::Null) => s.put("protected_solid", Value::Null),
        Some(m) => {
            let mask = mask_of(m, &s.at("protected_solid"), grid.shape)?;
            for (v, flag) in mask.iter().enumerate() {
                if *flag {
                    if region[v] {
                        return refuse(
                            "design.protected_solid overlaps design.region (protected voxels are not designable)",
                        );
                    }
                    protected_density[v] = 1.0;
                }
            }
            s.put("protected_solid", encode_mask(&mask, grid.shape));
        }
    }
    let initial_density = match s.raw("initial_density") {
        None => {
            s.put("initial_density", json!(0.5));
            vec![0.5; n]
        }
        Some(Value::Number(_)) => {
            let x = s.number("initial_density", |x| (0.0..=1.0).contains(&x), "in [0, 1]")?;
            vec![x; n]
        }
        Some(other) => {
            let list = match other {
                Value::Object(_) => values_of(other, &s.at("initial_density"), grid.shape, 0.0)?,
                _ => s.numbers_of("initial_density", other, Some(n))?,
            };
            if list.iter().any(|x| !(0.0..=1.0).contains(x)) {
                return refuse("design.initial_density values must lie in [0, 1]");
            }
            s.put("initial_density", json!(list));
            list
        }
    };
    let mut region = region;
    let (removal, removal_normal) =
        removal::parse(s.raw("removal"), grid, &initial_density, &mut region, &mut protected_density)?;
    s.put("removal", removal_normal);


    let initial_density = match &removal {
        Some(r) => initial_density
            .iter()
            .zip(&r.removable)
            .map(|(&x, &removable)| if removable { 1.0 } else { x })
            .collect(),
        None => initial_density,
    };
    let filter_radius_m = s.number_or("filter_radius_m", 0.0, |x| x >= 0.0, "nonnegative")?;
    let projection = if s.raw("projection").is_some_and(|v| !v.is_null()) {
        let mut p = s.child("projection", &["beta", "eta"])?;
        let out = (
            p.number("beta", |x| x > 0.0, "positive")?,
            p.number_or("eta", 0.5, |x| x > 0.0 && x < 1.0, "in (0, 1)")?,
        );
        s.put("projection", p.finish());
        Some(out)
    } else {
        s.put("projection", Value::Null);
        None
    };
    let interpolation = interpolation(&mut s)?;
    let mut b = s.child_or_empty("fluid_blocking", &["kind", "beta", "eta"])?;
    let constant_blocking = b.choice_or("kind", "projection", &["projection", "constant"])? == "constant";
    let blocking = if constant_blocking {
        if b.has("beta") || b.has("eta") {
            return refuse("design.fluid_blocking beta and eta apply to kind projection only");
        }
        BlockingMap::identity()
    } else {
        BlockingMap::new(
            b.number_or("beta", 8.0, |x| x >= 0.0, "nonnegative")?,
            b.number_or("eta", 0.5, |x| x > 0.0 && x < 1.0, "in (0, 1)")?,
        )?
    };
    s.put("fluid_blocking", b.finish());
    let mirror_axis = match s.raw("symmetry") {
        None | Some(Value::Null) => {
            s.put("symmetry", Value::Null);
            None
        }
        Some(_) => {
            let mut m = s.child("symmetry", &["mirror_axis"])?;
            let axis = m.integer("mirror_axis", 0..=2)?;
            s.put("symmetry", m.finish());
            let mirrored = |i: usize| mirror_voxel(grid, axis, i);
            if (0..n).any(|i| {
                region[i] != region[mirrored(i)]
                    || protected_density[i].to_bits() != protected_density[mirrored(i)].to_bits()
            }) {
                return refuse(format!(
                    "design.symmetry: the design region and protected densities are not mirror symmetric about the mid-plane of axis {axis}"
                ));
            }
            Some(axis)
        }
    };
    match s.raw("two_material") {
        None | Some(Value::Null) => s.put("two_material", Value::Null),
        Some(_) => {
            return refuse(
                "design.two_material is not available: the soft-solid model assigns one material per element and has no phase-interpolated stiffness E(rho, phi) (recorded as deferred scope in docs/HANDOFF.md); set it to null",
            );
        }
    }
    let spec = DesignSpec {
        region,
        protected_density,
        initial_density,
        filter_radius_m,
        projection,
        interpolation,
        blocking,
        constant_blocking,
        mirror_axis,
        removal,
    };
    Ok((spec, s.finish()))
}

#[must_use]
pub fn mirror_voxel(grid: &VoxelGrid, axis: usize, v: usize) -> usize {
    let s = grid.shape;
    let mut ijk = [v / (s[1] * s[2]), (v / s[2]) % s[1], v % s[2]];
    ijk[axis] = s[axis] - 1 - ijk[axis];
    (ijk[0] * s[1] + ijk[1]) * s[2] + ijk[2]
}

#[must_use]
pub fn voxel_centroids(grid: &VoxelGrid) -> Vec<[f64; 3]> {
    (0..grid.voxel_count()).map(|v| voxel_centroid(grid, v)).collect()
}
