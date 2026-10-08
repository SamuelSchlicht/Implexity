// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::soft::laws::Material;
use implexity_physics_solid::soft_fsi::voxel::VoxelGrid;

use crate::json::{Section, refuse, strip_nulls};
use crate::problem::design::{selection, voxel_centroids};
use crate::problem::material_map::scaled_law;

pub const REMOVAL_KEYS: [&str; 5] =
    ["reference", "max_depth_m", "forbidden", "open_faces", "depth_sharpness_m"];
pub const MODIFIER_KEYS: [&str; 6] =
    ["band_m", "saturation", "stiffness_factor", "material", "fibre_directions", "label"];
pub const FACES: [&str; 6] = ["xmin", "xmax", "ymin", "ymax", "zmin", "zmax"];
pub const REMOVED_VOLUME_FRACTION: &str = "removed_volume_fraction";
pub const REMOVED_VOLUME: &str = "removed_volume_m3";
pub const REMOVAL_DEPTH: &str = "removal_depth_m";
pub const REMOVAL_TERMS: [&str; 3] = [REMOVED_VOLUME_FRACTION, REMOVED_VOLUME, REMOVAL_DEPTH];

#[derive(Clone, Debug, PartialEq)]
pub struct RemovalSpec {
    pub reference: Vec<f64>,
    pub depth_m: Vec<f64>,
    pub max_depth_m: Option<f64>,
    pub forbidden: Vec<bool>,
    pub removable: Vec<bool>,
    pub depth_sharpness_m: f64,
    pub voxel_volume_m3: f64,
}

fn depths(grid: &VoxelGrid, reference: &[f64], open: [bool; 6]) -> Vec<f64> {
    let c = voxel_centroids(grid);
    let h = grid.element_size_m;
    let void: Vec<usize> = (0..reference.len()).filter(|&v| reference[v] <= 0.5).collect();
    let lo = grid.origin_m;
    let hi: [f64; 3] = std::array::from_fn(|a| grid.origin_m[a] + grid.shape[a] as f64 * h);
    (0..reference.len())
        .map(|v| {
            if reference[v] <= 0.5 {
                return 0.0;
            }
            let mut best = f64::INFINITY;
            for &w in &void {
                let d =
                    ((c[v][0] - c[w][0]).powi(2) + (c[v][1] - c[w][1]).powi(2) + (c[v][2] - c[w][2]).powi(2))
                        .sqrt()
                        - 0.5 * h;
                best = best.min(d);
            }
            for a in 0..3 {
                if open[2 * a] {
                    best = best.min(c[v][a] - lo[a]);
                }
                if open[2 * a + 1] {
                    best = best.min(hi[a] - c[v][a]);
                }
            }
            best
        })
        .collect()
}


pub fn parse(
    v: Option<&Value>,
    grid: &VoxelGrid,
    initial_density: &[f64],
    region: &mut [bool],
    protected_density: &mut [f64],
) -> CaeResult<(Option<RemovalSpec>, Value)> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok((None, Value::Null));
    };
    let mut s = Section::new(v, "design.removal", &REMOVAL_KEYS)?;
    match s.raw("reference") {
        None => s.put("reference", json!("initial_density")),
        Some(Value::String(t)) if t == "initial_density" => s.put("reference", json!("initial_density")),
        Some(_) => {
            return refuse("design.removal.reference must be \"initial_density\" (the reference occupancy)");
        }
    }
    let h = grid.element_size_m;
    let max_depth_m = match s.raw("max_depth_m") {
        None => {
            s.put("max_depth_m", Value::Null);
            None
        }
        Some(_) => Some(s.number("max_depth_m", |x| x > 0.0, "positive")?),
    };
    let n = grid.voxel_count();
    let forbidden = match s.raw("forbidden") {
        None => {
            s.put("forbidden", Value::Null);
            vec![false; n]
        }
        Some(f) => {
            let (flags, normal) = selection(f, &s.at("forbidden"), grid)?;
            s.put("forbidden", normal);
            flags
        }
    };
    let mut open = [false; 6];
    match s.raw("open_faces") {
        None => s.put("open_faces", json!([])),
        Some(Value::Array(a)) => {
            let mut names = Vec::new();
            for f in a {
                let t = f.as_str().unwrap_or_default();
                let Some(k) = FACES.iter().position(|x| *x == t) else {
                    return refuse(format!("design.removal.open_faces entries must be {}", FACES.join(", ")));
                };
                if !open[k] {
                    open[k] = true;
                    names.push(t.to_string());
                }
            }
            s.put("open_faces", json!(names));
        }
        Some(_) => return refuse("design.removal.open_faces must be a list of grid faces"),
    }
    let depth_sharpness_m = s.number_or("depth_sharpness_m", 0.25 * h, |x| x > 0.0, "positive")?;
    let reference = initial_density.to_vec();
    let depth_m = depths(grid, &reference, open);
    let mut removable = vec![false; n];
    for v in 0..n {
        removable[v] = region[v]
            && reference[v] > 0.0
            && !forbidden[v]
            && max_depth_m.is_none_or(|d| depth_m[v] <= d + 1e-12 * h);
        if !removable[v] {

            region[v] = false;
            protected_density[v] = reference[v];
        }
    }
    if !removable.iter().any(|b| *b) {
        return refuse(
            "design.removal leaves no removable voxel (region ∩ reference material ∖ forbidden within max_depth_m)",
        );
    }
    let spec = RemovalSpec {
        reference,
        depth_m,
        max_depth_m,
        forbidden,
        removable,
        depth_sharpness_m,
        voxel_volume_m3: h * h * h,
    };
    Ok((Some(spec), s.finish()))
}

impl RemovalSpec {
    #[must_use]
    pub fn removal(&self, density: &[f64]) -> Vec<f64> {
        self.reference.iter().zip(density).map(|(x, r)| x - r).collect()
    }


    pub fn term(&self, kind: &str, density: &[f64]) -> CaeResult<(f64, Vec<f64>)> {
        if density.len() != self.reference.len() {
            return refuse("removal design term: density of the wrong length");
        }
        let r = self.removal(density);
        let n = r.len();
        match kind {
            REMOVED_VOLUME_FRACTION => {
                let total: f64 = self.reference.iter().sum();
                if total <= 0.0 {
                    return refuse("removed_volume_fraction: the reference holds no material");
                }
                Ok((r.iter().sum::<f64>() / total, vec![-1.0 / total; n]))
            }
            REMOVED_VOLUME => {
                let v = self.voxel_volume_m3;
                Ok((r.iter().sum::<f64>() * v, vec![-v; n]))
            }
            REMOVAL_DEPTH => {
                let delta = self.depth_sharpness_m;
                let set: Vec<usize> = (0..n).filter(|&v| self.removable[v]).collect();
                let z: Vec<f64> = set.iter().map(|&v| self.depth_m[v] * r[v] / delta).collect();
                let shift = z.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let e: Vec<f64> = z.iter().map(|x| (x - shift).exp()).collect();
                let sum: f64 = e.iter().sum();
                let value = delta * (shift + (sum / set.len() as f64).ln());
                let mut g = vec![0.0; n];
                for (k, &v) in set.iter().enumerate() {
                    g[v] = -self.depth_m[v] * e[k] / sum;
                }
                Ok((value, g))
            }
            other => Err(CaeError::contract(format!("unknown removal design term {other:?}"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModifierSpec {
    pub label: String,
    pub band_m: f64,
    pub saturation: f64,
    pub stiffness_factor: f64,
    pub sign: f64,
    pub law: Value,
    pub material: Material,
    pub fibre_directions: Vec<[f64; 3]>,
}


pub fn parse_modifier(
    v: Option<&Value>,
    base_law: &Value,
    grid: &VoxelGrid,
) -> CaeResult<(Option<ModifierSpec>, Value)> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok((None, Value::Null));
    };
    let mut s = Section::new(v, "solid.removal_modifier", &MODIFIER_KEYS)?;
    let label = s.text_or("label", "removal_zone", 64)?;
    let h = grid.element_size_m;
    let band_m = s.number("band_m", |x| x > 0.0, "positive")?;
    if band_m < 0.5 * h {
        return refuse(format!(
            "solid.removal_modifier.band_m {band_m} m is below half a voxel ({} m): the band holds only its own voxel",
            0.5 * h
        ));
    }
    let saturation = s.number_or("saturation", 0.1, |x| x > 0.0 && x <= 1.0, "in (0, 1]")?;
    let (stiffness_factor, sign, law) = if let Some(m) = s.raw("material") {
        if s.has("stiffness_factor") {
            return refuse(
                "solid.removal_modifier: give either stiffness_factor or an added material, not both",
            );
        }
        s.put("stiffness_factor", Value::Null);
        let law = strip_nulls(m);
        s.put("material", law.clone());
        (f64::NAN, 1.0, law)
    } else {
        let k = s.number_or("stiffness_factor", 3.0, |x| x > 0.0, "positive")?;
        if (k - 1.0).abs() < 1e-12 {
            return refuse("solid.removal_modifier.stiffness_factor 1 modifies nothing; remove the modifier");
        }
        s.put("material", Value::Null);
        let law = scaled_law(base_law, (k - 1.0).abs(), 1.0, Some(&Value::Null))?;
        (k, (k - 1.0).signum(), law)
    };
    let material = implexity_physics_solid::soft::problem::material(&law)
        .map_err(|e| e.context("solid.removal_modifier material"))?;
    if material.coupled() {
        return refuse(
            "solid.removal_modifier: the added law must be decoupled (isochoric part plus a volumetric function); the \
             modifier adds only its isochoric energy",
        );
    }
    let fibre_directions = if material.fibres.is_some() {
        let rows = s
            .raw("fibre_directions")
            .and_then(Value::as_array)
            .filter(|a| (1..=4).contains(&a.len()))
            .ok_or_else(|| {
                CaeError::contract(
                    "solid.removal_modifier.fibre_directions must list 1..4 XYZ directions for a fibre law",
                )
            })?;
        let mut out = Vec::new();
        for r in rows {
            let t = s.numbers_of("fibre_directions", r, Some(3))?;
            let n = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
            if !(n.is_finite() && n > 0.0) {
                return refuse("solid.removal_modifier.fibre_directions must be nonzero vectors");
            }
            out.push([t[0] / n, t[1] / n, t[2] / n]);
        }
        s.put("fibre_directions", json!(out));
        out
    } else {
        if s.has("fibre_directions") {
            return refuse("solid.removal_modifier.fibre_directions requires an added law with fibres");
        }
        s.put("fibre_directions", Value::Null);
        Vec::new()
    };
    let spec =
        ModifierSpec { label, band_m, saturation, stiffness_factor, sign, law, material, fibre_directions };
    Ok((Some(spec), s.finish()))
}
