// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::soft::laws::Material;
use implexity_physics_solid::soft::model::Formulation;
use implexity_physics_solid::soft::stepper::{FactorizationReuse, NewtonOptions, Scheme};
use implexity_physics_solid::soft_fsi::voxel::VoxelGrid;

use crate::json::{Section, box_of, encode_mask, kind_of, mask_of, refuse, strip_nulls};
use crate::problem::material_map::{self, MaterialMap};
use crate::problem::removal::{self as removal_zone, ModifierSpec};

pub const SOLID_KEYS: [&str; 17] = [
    "reference_grid",
    "plane_strain",
    "material",
    "material_map",
    "removal_modifier",
    "fibre_directions",
    "formulation",
    "stabilization",
    "mass",
    "rayleigh",
    "supports",
    "contact_planes",
    "foundations",
    "integrator",
    "newton",
    "inertia_compensation",
    "provenance",
];

pub const INTEGRATORS: [&str; 4] = ["avf_midpoint", "generalized_alpha", "newmark", "quasistatic"];

#[derive(Clone, Debug, PartialEq)]
pub struct ContactPlane {
    pub normal: [f64; 3],
    pub offset_m: f64,
    pub activation_m: f64,
    pub stiffness_pa: f64,
    pub region: Option<Vec<bool>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeSelection {
    Box([[f64; 3]; 2]),
    Voxels(Vec<bool>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Motion {
    Harmonic {
        amplitude_m: [f64; 3],
        frequency_hz: f64,
        phase_rad: f64,
    },
    HarmonicRotation {
        axis: [f64; 3],
        centre_m: [f64; 3],
        amplitude_rad: f64,
        frequency_hz: f64,
        phase_rad: f64,
    },
}

impl Motion {
    #[must_use]
    pub fn signal(&self) -> (f64, f64) {
        match self {
            Self::Harmonic { frequency_hz, phase_rad, .. }
            | Self::HarmonicRotation { frequency_hz, phase_rad, .. } => (*frequency_hz, *phase_rad),
        }
    }

    #[must_use]
    pub fn pattern(&self, x: [f64; 3]) -> [f64; 3] {
        match self {
            Self::Harmonic { amplitude_m, .. } => *amplitude_m,
            Self::HarmonicRotation { axis, centre_m, amplitude_rad, .. } => {
                let r = [x[0] - centre_m[0], x[1] - centre_m[1], x[2] - centre_m[2]];
                [
                    amplitude_rad * (axis[1] * r[2] - axis[2] * r[1]),
                    amplitude_rad * (axis[2] * r[0] - axis[0] * r[2]),
                    amplitude_rad * (axis[0] * r[1] - axis[1] * r[0]),
                ]
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Support {
    pub selection: NodeSelection,
    pub components: [bool; 3],
    pub motion: Option<Motion>,
}

fn motion_of(v: &Value, path: &str) -> CaeResult<(Motion, Value)> {
    let kind = kind_of(v, path, &["harmonic", "harmonic_rotation"])?;
    if kind == "harmonic" {
        let mut m = Section::new(v, path, &["kind", "amplitude_m", "frequency_hz", "phase_rad"])?;
        m.put("kind", json!("harmonic"));
        let motion = Motion::Harmonic {
            amplitude_m: m.triple("amplitude_m")?,
            frequency_hz: m.number("frequency_hz", positive, "positive")?,
            phase_rad: m.number_or("phase_rad", 0.0, |_| true, "finite")?,
        };
        return Ok((motion, m.finish()));
    }
    let mut m =
        Section::new(v, path, &["kind", "axis", "centre_m", "amplitude_rad", "frequency_hz", "phase_rad"])?;
    m.put("kind", json!("harmonic_rotation"));
    let axis = unit(m.triple_or("axis", [0.0, 0.0, 1.0])?, &m.at("axis"))?;
    m.put("axis", json!(axis));
    let motion = Motion::HarmonicRotation {
        axis,
        centre_m: m.triple("centre_m")?,
        amplitude_rad: m.number(
            "amplitude_rad",
            |x| x.abs() <= 0.35,
            "a small angle (|theta| <= 0.35 rad, linearised rotation)",
        )?,
        frequency_hz: m.number("frequency_hz", positive, "positive")?,
        phase_rad: m.number_or("phase_rad", 0.0, |_| true, "finite")?,
    };
    Ok((motion, m.finish()))
}

#[derive(Clone, Debug)]
pub struct SolidSpec {
    pub grid: VoxelGrid,
    pub plane_strain: bool,
    pub material: Material,
    pub fibre_directions: Vec<[f64; 3]>,
    pub formulation: Formulation,
    pub lumped_mass: bool,
    pub rayleigh: (f64, f64),
    pub supports: Vec<Support>,
    pub contact_planes: Vec<ContactPlane>,
    pub foundations: Vec<([[f64; 3]; 2], f64)>,
    pub scheme: Scheme,
    pub newton: NewtonOptions,
    pub factorization_reuse: Option<FactorizationReuse>,
    pub inertia_compensation: f64,
    pub material_density_kg_m3: f64,
    pub material_map: MaterialMap,
    pub removal_modifier: Option<ModifierSpec>,
}

fn positive(x: f64) -> bool {
    x > 0.0
}

fn unit(v: [f64; 3], what: &str) -> CaeResult<[f64; 3]> {
    let n = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    if !(n.is_finite() && n > 0.0) {
        return refuse(format!("{what} must be a nonzero vector"));
    }
    Ok(v.map(|x| x / n))
}

fn scheme(s: &mut Section<'_>) -> CaeResult<Scheme> {
    let v = s.raw("integrator").cloned().unwrap_or_else(|| json!({"kind": "avf_midpoint"}));
    let path = s.at("integrator");
    let kind = kind_of(&v, &path, &INTEGRATORS)?;
    let (scheme, normal) = match kind {
        "avf_midpoint" => {
            let mut c = Section::new(&v, &path, &["kind", "gauss_points"])?;
            c.put("kind", json!("avf_midpoint"));
            let g = c.integer_or("gauss_points", 3, 1..=8)?;
            (Scheme::AvfMidpoint { gauss_points: g }, c.finish())
        }
        "generalized_alpha" => {
            let mut c = Section::new(&v, &path, &["kind", "rho_inf"])?;
            c.put("kind", json!("generalized_alpha"));
            let r = c.number_or("rho_inf", 0.8, |x| (0.0..=1.0).contains(&x), "in [0, 1]")?;
            (Scheme::GeneralizedAlpha { rho_inf: r }, c.finish())
        }
        "newmark" => {
            let mut c = Section::new(&v, &path, &["kind", "beta", "gamma"])?;
            c.put("kind", json!("newmark"));
            let beta = c.number_or("beta", 0.25, |x| x > 0.0 && x <= 0.5, "in (0, 0.5]")?;
            let gamma = c.number_or("gamma", 0.5, |x| (0.5..=1.0).contains(&x), "in [0.5, 1]")?;
            (Scheme::Newmark { beta, gamma }, c.finish())
        }
        _ => {
            Section::new(&v, &path, &["kind"])?;
            (Scheme::Quasistatic, json!({"kind": "quasistatic"}))
        }
    };
    s.put("integrator", normal);
    Ok(scheme)
}


#[allow(clippy::too_many_lines)]
pub fn parse(v: &Value, fluid_density: f64) -> CaeResult<(SolidSpec, Value)> {
    let mut s = Section::new(v, "solid", &SOLID_KEYS)?;
    let mut g = s.child("reference_grid", &["origin_m", "shape", "element_size_m"])?;
    let origin = g.triple_or("origin_m", [0.0; 3])?;
    let shape = g.shape("shape", 1024)?;
    let size = g.number("element_size_m", positive, "positive")?;
    let grid = VoxelGrid::new(origin, shape, size)?;
    if grid.voxel_count() > 200_000 {
        return refuse("solid.reference_grid exceeds 200000 voxels (1.2e6 tetrahedra)");
    }
    s.put("reference_grid", g.finish());
    let plane_strain = s.boolean_or("plane_strain", shape[2] == 1)?;
    if plane_strain && shape[2] != 1 {
        return refuse("solid.plane_strain needs exactly one voxel layer along z");
    }
    let mv = strip_nulls(s.raw("material").ok_or_else(|| CaeError::contract("solid.material is required"))?);
    let mut material =
        implexity_physics_solid::soft::problem::material(&mv).map_err(|e| e.context("solid.material"))?;
    s.put("material", mv.clone());
    let (mut material_map, map_normal) = material_map::parse(s.raw("material_map"), &mv, &material, &grid)?;
    s.put("material_map", map_normal);
    let (removal_modifier, modifier_normal) =
        removal_zone::parse_modifier(s.raw("removal_modifier"), &mv, &grid)?;
    s.put("removal_modifier", modifier_normal);
    let fibre_directions = if material_map.materials.iter().any(|m| m.fibres.is_some()) {
        let raw = s.raw("fibre_directions").ok_or_else(|| {
            CaeError::contract("solid.fibre_directions is required for a fibre-reinforced material")
        })?;
        let rows = raw
            .as_array()
            .filter(|a| (1..=4).contains(&a.len()))
            .ok_or_else(|| CaeError::contract("solid.fibre_directions must list 1..4 XYZ directions"))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let t = s.numbers_of("fibre_directions", r, Some(3))?;
            out.push(unit([t[0], t[1], t[2]], "solid.fibre_directions")?);
        }
        s.put("fibre_directions", json!(out));
        out
    } else {
        if s.has("fibre_directions") {
            return refuse("solid.fibre_directions requires a material with fibres");
        }
        Vec::new()
    };
    let formulation = match s.choice_or("formulation", "mixed_up", &["displacement", "mixed_up"])?.as_str() {
        "displacement" => {
            if s.has("stabilization") {
                return refuse("solid.stabilization applies to the mixed_up formulation only");
            }
            Formulation::Displacement
        }
        _ => Formulation::Mixed { stabilization: s.number_or("stabilization", 1.0, positive, "positive")? },
    };
    let lumped_mass = s.choice_or("mass", "consistent", &["consistent", "lumped"])? == "lumped";
    let mut r = s.child_or_empty("rayleigh", &["alpha_mass_s_inv", "beta_stiffness_s"])?;
    let rayleigh = (
        r.number_or("alpha_mass_s_inv", 0.0, |x| x >= 0.0, "nonnegative")?,
        r.number_or("beta_stiffness_s", 0.0, |x| x >= 0.0, "nonnegative")?,
    );
    s.put("rayleigh", r.finish());
    let mut supports = Vec::new();
    let mut normal = Vec::new();
    for (v, path) in s.list("supports", 64)? {
        let mut c = Section::new(v, &path, &["box_m", "region", "components", "motion"])?;
        let selection = match (c.raw("box_m"), c.raw("region")) {
            (Some(b), None) => {
                let b = box_of(b, &c.at("box_m"))?;
                c.put("box_m", json!([b[0], b[1]]));
                NodeSelection::Box(b)
            }
            (None, Some(r)) => {
                let m = mask_of(r, &c.at("region"), grid.shape)?;
                c.put("region", encode_mask(&m, grid.shape));
                NodeSelection::Voxels(m)
            }
            _ => {
                return refuse(format!(
                    "{path} needs exactly one of box_m (nodes in a box) and region (nodes of a voxel mask)"
                ));
            }
        };
        let components = match c.raw("components") {
            None => [true; 3],
            Some(Value::Array(a)) if a.len() == 3 && a.iter().all(Value::is_boolean) => {
                [a[0] == Value::Bool(true), a[1] == Value::Bool(true), a[2] == Value::Bool(true)]
            }
            Some(Value::Array(a)) if a.len() <= 3 && a.iter().all(|x| x.as_u64().is_some_and(|i| i < 3)) => {
                let mut f = [false; 3];
                for x in a {
                    #[allow(clippy::cast_possible_truncation)]
                    let i = x.as_u64().unwrap_or(0) as usize;
                    f[i] = true;
                }
                f
            }
            Some(_) => {
                return refuse(format!(
                    "{path}.components must be three Booleans or a list of component indices 0..2"
                ));
            }
        };
        if !components.iter().any(|b| *b) {
            return refuse(format!("{path}.components fixes no component"));
        }
        c.put("components", json!(components));
        let motion = match c.raw("motion") {
            None | Some(Value::Null) => {
                c.put("motion", Value::Null);
                None
            }
            Some(m) => {
                let (motion, mn) = motion_of(m, &c.at("motion"))?;
                c.put("motion", mn);
                Some(motion)
            }
        };
        supports.push(Support { selection, components, motion });
        normal.push(c.finish());
    }
    s.put("supports", Value::Array(normal));
    let mut contact_planes = Vec::new();
    let mut normal = Vec::new();
    for (v, path) in s.list("contact_planes", 8)? {
        let mut c =
            Section::new(v, &path, &["normal", "offset_m", "activation_m", "stiffness_pa", "region"])?;
        let n = unit(c.triple("normal")?, &c.at("normal"))?;
        c.put("normal", json!(n));
        let region = match c.raw("region") {
            None => None,
            Some(r) => {
                let m = mask_of(r, &c.at("region"), grid.shape)?;
                if !m.iter().any(|v| *v) {
                    return refuse(format!("{path}.region flags no voxel"));
                }
                c.put("region", encode_mask(&m, grid.shape));
                Some(m)
            }
        };
        contact_planes.push(ContactPlane {
            normal: n,
            offset_m: c.number("offset_m", |_| true, "finite")?,
            activation_m: c.number("activation_m", positive, "positive")?,
            stiffness_pa: c.number("stiffness_pa", positive, "positive")?,
            region,
        });
        normal.push(c.finish());
    }
    s.put("contact_planes", Value::Array(normal));
    let mut foundations = Vec::new();
    let mut normal = Vec::new();
    for (v, path) in s.list("foundations", 16)? {
        let mut c = Section::new(v, &path, &["box_m", "stiffness_n_m"])?;
        let b = box_of(c.raw("box_m").unwrap_or(&Value::Null), &c.at("box_m"))?;
        c.put("box_m", json!([b[0], b[1]]));
        foundations.push((b, c.number("stiffness_n_m", |x| x >= 0.0, "nonnegative")?));
        normal.push(c.finish());
    }
    s.put("foundations", Value::Array(normal));
    let scheme = scheme(&mut s)?;
    let mut nw =
        s.child_or_empty("newton", &["max_iterations", "relative_tolerance", "factorization_reuse"])?;
    let newton = NewtonOptions {
        max_iterations: nw.integer_or("max_iterations", 25, 1..=500)?,
        relative_tolerance: nw.number_or("relative_tolerance", 1e-10, |x| x > 0.0 && x < 1.0, "in (0, 1)")?,
    };
    let factorization_reuse = match nw.raw("factorization_reuse") {
        None | Some(Value::Bool(false)) => {
            nw.put("factorization_reuse", json!(false));
            None
        }
        Some(Value::Bool(true)) => {
            let d = FactorizationReuse::default();
            nw.put("factorization_reuse", json!({"contraction": d.contraction, "max_reuse": d.max_reuse}));
            Some(d)
        }
        Some(_) => {
            let mut r = nw.child("factorization_reuse", &["contraction", "max_reuse"])?;
            let d = FactorizationReuse::default();
            let reuse = FactorizationReuse {
                contraction: r.number_or(
                    "contraction",
                    d.contraction,
                    |x| x > 0.0 && x < 1.0,
                    "in (0, 1)",
                )?,
                max_reuse: r.integer_or("max_reuse", d.max_reuse, 1..=100)?,
            };
            nw.put("factorization_reuse", r.finish());
            Some(reuse)
        }
    };
    s.put("newton", nw.finish());
    let inertia_compensation =
        s.number_or("inertia_compensation", 0.0, |x| (0.0..=1.0).contains(&x), "in [0, 1]")?;
    let material_density_kg_m3 = material.density;
    if inertia_compensation > 0.0 {
        let reduced = material.density - inertia_compensation * fluid_density;
        if reduced.partial_cmp(&(0.05 * material.density)) != Some(std::cmp::Ordering::Greater) {
            return refuse(format!(
                "solid.inertia_compensation {inertia_compensation} removes {:.4e} kg/m3 of fluid inertia from a \
                 solid of {:.4e} kg/m3 and leaves less than 5 % of its mass; use a strong coupling mode without \
                 compensation (the fluid inside the body then adds its inertia, reported in the record)",
                inertia_compensation * fluid_density,
                material.density
            ));
        }
        material.density = reduced;
        for (k, m) in material_map.materials.iter_mut().enumerate().skip(1) {
            let reduced = m.density - inertia_compensation * fluid_density;
            if reduced.partial_cmp(&(0.05 * m.density)) != Some(std::cmp::Ordering::Greater) {
                return refuse(format!(
                    "solid.inertia_compensation {inertia_compensation} leaves less than 5 % of the mass of \
                     solid.material_map entry {:?}",
                    material_map.labels[k - 1]
                ));
            }
            m.density = reduced;
        }
    }
    material_map.materials[0] = material.clone();
    s.text_or("provenance", "", 2000)?;
    let spec = SolidSpec {
        grid,
        plane_strain,
        material,
        fibre_directions,
        formulation,
        lumped_mass,
        rayleigh,
        supports,
        contact_planes,
        foundations,
        scheme,
        newton,
        factorization_reuse,
        inertia_compensation,
        material_density_kg_m3,
        material_map,
        removal_modifier,
    };
    Ok((spec, s.finish()))
}
