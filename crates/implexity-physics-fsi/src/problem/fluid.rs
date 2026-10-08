// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_core::CaeResult;
use implexity_physics_lbm::moving::boundary::{Face, PortKind, PortSpec, Signal};
use implexity_physics_lbm::moving::collision::{CollisionKind, MAGIC_DEFAULT};
use implexity_physics_lbm::moving::field::{EntrainedInertia, LatticeKind, Sampling, Turbulence};
use implexity_physics_lbm::moving::psm::{CouplingLaw, Smagorinsky};
use implexity_physics_lbm::moving::sponge::{SpongeSpec, TravellingWave};
use implexity_physics_lbm::moving::wale::Wale;

use crate::json::{Section, box_of, encode_mask, kind_of, mask_of, refuse};

pub const FLUID_KEYS: [&str; 25] = [
    "lattice",
    "shape",
    "spacing_m",
    "origin_m",
    "periodic_axes",
    "density_kg_m3",
    "kinematic_viscosity_m2_s",
    "collision",
    "turbulence",
    "coupling_law",
    "walls",
    "solid_mask",
    "ports",
    "sponges",
    "body_acceleration_m_s2",
    "initial_velocity_m_s",
    "initial_pressure_pa",
    "mach_limit",
    "lattice_velocity_limit",
    "tau_min",
    "sampling",
    "inner_checkpoint_bytes",
    "regime",
    "symmetry_faces",
    "entrained_inertia",
];

pub const LATTICES: [&str; 3] = ["D2Q9", "D3Q19", "D3Q27"];
pub const COLLISIONS: [&str; 5] = ["bgk", "trt", "mrt", "regularized", "cumulant"];
pub const TURBULENCE: [&str; 3] = ["laminar", "smagorinsky", "wale"];
pub const COUPLING_LAWS: [&str; 3] = ["psm_superposition", "psm", "brinkman"];
pub const REFUSED_COUPLING_LAWS: [(&str, &str); 2] = [
    (
        "interpolated_bounce_back",
        "interpolated bounce-back is a non-differentiable verification mode for prescribed rigid motion (implexity_physics_lbm::moving::ibb and ::verification); it cannot carry a deforming soft solid",
    ),
    (
        "immersed_boundary",
        "immersed-boundary coupling is not provided: with density-weighted markers it reduces to the brinkman law, and a surface-marker immersed boundary needs an explicit surface that a density design does not have (FSI_DYNAMIC_TOPOLOGY.md 3.2)",
    ),
];

#[derive(Clone, Debug)]
pub struct FluidSpec {
    pub lattice: LatticeKind,
    pub shape: [usize; 3],
    pub spacing_m: f64,
    pub origin_m: [f64; 3],
    pub periodic: [bool; 3],
    pub density_kg_m3: f64,
    pub kinematic_viscosity_m2_s: f64,
    pub collision: CollisionKind,
    pub turbulence: Turbulence,
    pub coupling_law: CouplingLaw,
    pub solid_mask: Vec<bool>,
    pub ports: Vec<PortSpec>,
    pub sponges: Vec<SpongeSpec>,
    pub body_acceleration_m_s2: [f64; 3],
    pub initial_velocity_m_s: [f64; 3],
    pub initial_pressure_pa: f64,
    pub mach_limit: f64,
    pub lattice_velocity_limit: f64,
    pub tau_min: f64,
    pub sampling: Sampling,
    pub inner_checkpoint_bytes: u64,
    pub regime: Option<RegimeDeclaration>,
    pub symmetry_faces: Vec<Face>,
    pub entrained_inertia: EntrainedInertia,
}

#[must_use]
pub fn wall_at(f: &FluidSpec, x: [f64; 3]) -> bool {
    let mut ijk = [0usize; 3];
    for a in 0..3 {
        let t = ((x[a] - f.origin_m[a]) / f.spacing_m).floor();
        if !(t >= 0.0 && t < f.shape[a] as f64) {
            return false;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            ijk[a] = t as usize;
        }
    }
    f.solid_mask[index(f.shape, ijk)]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegimeDeclaration {
    pub velocity_m_s: f64,
    pub length_m: f64,
    pub reynolds_max: f64,
}

fn positive(x: f64) -> bool {
    x > 0.0
}

fn face_of(s: &mut Section<'_>, key: &str) -> CaeResult<Face> {
    let name = s.choice(key, &["xmin", "xmax", "ymin", "ymax", "zmin", "zmax"])?;
    Face::parse(&name)
}

fn cell_center(origin: [f64; 3], dx: f64, ijk: [usize; 3]) -> [f64; 3] {
    std::array::from_fn(|a| origin[a] + (ijk[a] as f64 + 0.5) * dx)
}

fn index(shape: [usize; 3], ijk: [usize; 3]) -> usize {
    (ijk[0] * shape[1] + ijk[1]) * shape[2] + ijk[2]
}

fn coords(shape: [usize; 3], cell: usize) -> [usize; 3] {
    let k = cell % shape[2];
    let ij = cell / shape[2];
    [ij / shape[1], ij % shape[1], k]
}

fn walls(s: &mut Section<'_>, shape: [usize; 3], dx: f64, origin: [f64; 3]) -> CaeResult<Vec<bool>> {
    let n: usize = shape.iter().product();
    let mut mask = vec![false; n];
    let mut normal = Vec::new();
    for (v, path) in s.list("walls", 4096)? {
        let kind = kind_of(v, &path, &["box", "cylinder", "sphere", "cells"])?;
        match kind {
            "box" => {
                let mut w = Section::new(v, &path, &["kind", "box_m"])?;
                w.put("kind", json!("box"));
                let b = box_of(w.raw("box_m").unwrap_or(&Value::Null), &w.at("box_m"))?;
                w.put("box_m", json!([b[0], b[1]]));
                for (c, m) in mask.iter_mut().enumerate() {
                    let x = cell_center(origin, dx, coords(shape, c));
                    if (0..3).all(|a| x[a] >= b[0][a] && x[a] <= b[1][a]) {
                        *m = true;
                    }
                }
                normal.push(w.finish());
            }
            "cylinder" => {
                let mut w = Section::new(v, &path, &["kind", "center_m", "radius_m", "axis"])?;
                w.put("kind", json!("cylinder"));
                let c0 = w.triple("center_m")?;
                let r = w.number("radius_m", positive, "positive")?;
                let axis = w.integer_or("axis", 2, 0..=2)?;
                for (c, m) in mask.iter_mut().enumerate() {
                    let x = cell_center(origin, dx, coords(shape, c));
                    let d2: f64 = (0..3).filter(|a| *a != axis).map(|a| (x[a] - c0[a]).powi(2)).sum();
                    if d2 <= r * r {
                        *m = true;
                    }
                }
                normal.push(w.finish());
            }
            "sphere" => {
                let mut w = Section::new(v, &path, &["kind", "center_m", "radius_m"])?;
                w.put("kind", json!("sphere"));
                let c0 = w.triple("center_m")?;
                let r = w.number("radius_m", positive, "positive")?;
                for (c, m) in mask.iter_mut().enumerate() {
                    let x = cell_center(origin, dx, coords(shape, c));
                    if (0..3).map(|a| (x[a] - c0[a]).powi(2)).sum::<f64>() <= r * r {
                        *m = true;
                    }
                }
                normal.push(w.finish());
            }
            _ => {
                let mut w = Section::new(v, &path, &["kind", "indices"])?;
                w.put("kind", json!("cells"));
                let list = w.raw("indices").and_then(Value::as_array).ok_or_else(|| {
                    implexity_core::CaeError::contract(format!("{path}.indices must list cell indices"))
                })?;
                let mut idx = Vec::with_capacity(list.len());
                for x in list {
                    let i = x.as_u64().and_then(|i| usize::try_from(i).ok()).filter(|i| *i < n).ok_or_else(
                        || {
                            implexity_core::CaeError::contract(format!(
                                "{path}.indices must be lattice cell indices below {n}"
                            ))
                        },
                    )?;
                    mask[i] = true;
                    idx.push(i);
                }
                w.put("indices", json!(idx));
                normal.push(w.finish());
            }
        }
    }
    s.put("walls", Value::Array(normal));
    Ok(mask)
}

fn face_cells(shape: [usize; 3], mask: &[bool], face: Face) -> Vec<usize> {
    let axis = face.axis();
    let layer = face.layer(shape);
    (0..mask.len()).filter(|&c| coords(shape, c)[axis] == layer && !mask[c]).collect()
}

fn parabolic(shape: [usize; 3], periodic: [bool; 3], face: Face, cells: &[usize]) -> Vec<f64> {
    let axis = face.axis();
    cells
        .iter()
        .map(|&c| {
            let ijk = coords(shape, c);
            (0..3)
                .filter(|a| *a != axis && !periodic[*a] && shape[*a] > 1)
                .map(|a| {
                    let s = (ijk[a] as f64 + 0.5) / shape[a] as f64;
                    4.0 * s * (1.0 - s)
                })
                .product()
        })
        .collect()
}

fn signal(s: &mut Section<'_>) -> CaeResult<Signal> {
    Ok(Signal {
        frequency_hz: s.number_or("frequency_hz", 0.0, |x| x >= 0.0, "nonnegative")?,
        phase_rad: s.number_or("phase_rad", 0.0, |_| true, "finite")?,
        ramp_s: s.number_or("ramp_s", 0.0, |x| x >= 0.0, "nonnegative")?,
    })
}

fn ports(
    s: &mut Section<'_>,
    shape: [usize; 3],
    periodic: [bool; 3],
    mask: &[bool],
) -> CaeResult<Vec<PortSpec>> {
    let mut out = Vec::new();
    let mut normal = Vec::new();
    for (v, path) in s.list("ports", 6)? {
        let kind = kind_of(v, &path, &["velocity", "pressure"])?;
        let common = ["kind", "id", "face", "frequency_hz", "phase_rad", "ramp_s"];
        let mut keys: Vec<&str> = common.to_vec();
        if kind == "velocity" {
            keys.extend(["mean_m_s", "amplitude_m_s", "profile"]);
        } else {
            keys.extend(["pressure_pa", "amplitude_pa"]);
        }
        let mut p = Section::new(v, &path, &keys)?;
        p.put("kind", json!(kind));
        let id = p.text_or("id", &format!("port{}", out.len()), 64)?;
        if id.is_empty() || out.iter().any(|q: &PortSpec| q.id == id) {
            return refuse(format!("{path}.id must be a unique nonempty text"));
        }
        let face = face_of(&mut p, "face")?;
        if periodic[face.axis()] {
            return refuse(format!("{path}.face lies on a periodic lattice axis"));
        }
        let cells = face_cells(shape, mask, face);
        let port_kind = if kind == "velocity" {
            let mean = p.triple_or("mean_m_s", [0.0; 3])?;
            let amplitude = p.triple_or("amplitude_m_s", [0.0; 3])?;
            let profile = match p.raw("profile") {
                None => {
                    p.put("profile", json!("uniform"));
                    Vec::new()
                }
                Some(Value::String(t)) if t == "uniform" => {
                    p.put("profile", json!("uniform"));
                    Vec::new()
                }
                Some(Value::String(t)) if t == "parabolic" => {
                    p.put("profile", json!("parabolic"));
                    parabolic(shape, periodic, face, &cells)
                }
                Some(other) => {
                    let f = p.numbers_of("profile", other, Some(cells.len()))?;
                    p.put("profile", json!(f));
                    f
                }
            };
            PortKind::Velocity { mean_m_s: mean, amplitude_m_s: amplitude, profile }
        } else {
            PortKind::Pressure {
                mean_pa: p.number_or("pressure_pa", 0.0, |_| true, "finite")?,
                amplitude_pa: p.number_or("amplitude_pa", 0.0, |_| true, "finite")?,
            }
        };
        let signal = signal(&mut p)?;
        out.push(PortSpec { id, face, cells, kind: port_kind, signal });
        normal.push(p.finish());
    }
    s.put("ports", Value::Array(normal));
    Ok(out)
}

fn sponges(s: &mut Section<'_>, periodic: [bool; 3]) -> CaeResult<Vec<SpongeSpec>> {
    let mut out = Vec::new();
    let mut normal = Vec::new();
    for (v, path) in s.list("sponges", 6)? {
        let mut p = Section::new(
            v,
            &path,
            &["face", "thickness_cells", "strength", "pressure_pa", "velocity_m_s", "wave"],
        )?;
        let face = face_of(&mut p, "face")?;
        if periodic[face.axis()] {
            return refuse(format!("{path}.face lies on a periodic lattice axis"));
        }
        let thickness_cells = p.integer("thickness_cells", 1..=4096)?;
        let strength = p.number("strength", |x| x > 0.0 && x < 1.0, "in (0, 1)")?;
        let pressure_pa = p.number_or("pressure_pa", 0.0, |_| true, "finite")?;
        let velocity_m_s = p.triple_or("velocity_m_s", [0.0; 3])?;
        let wave = if p.raw("wave").is_some_and(|w| !w.is_null()) {
            let mut w = p.child(
                "wave",
                &[
                    "amplitude_m_s",
                    "frequency_hz",
                    "phase_rad",
                    "ramp_s",
                    "direction",
                    "speed_m_s",
                    "origin_m",
                ],
            )?;
            let direction = w.triple("direction")?;
            let norm = direction.iter().map(|d| d * d).sum::<f64>().sqrt();
            if !(norm.is_finite() && norm > 0.0) {
                return refuse(format!("{path}.wave.direction must be a nonzero vector"));
            }
            let wave = TravellingWave {
                amplitude_m_s: w.triple("amplitude_m_s")?,
                signal: Signal {
                    frequency_hz: w.number("frequency_hz", |x| x > 0.0, "positive")?,
                    phase_rad: w.number_or("phase_rad", 0.0, |_| true, "finite")?,
                    ramp_s: w.number_or("ramp_s", 0.0, |x| x >= 0.0, "nonnegative")?,
                },
                direction: direction.map(|d| d / norm),
                speed_m_s: w.number("speed_m_s", |x| x > 0.0, "positive")?,
                origin_m: w.triple_or("origin_m", [0.0; 3])?,
            };
            w.put("direction", json!(wave.direction));
            p.put("wave", w.finish());
            Some(wave)
        } else {
            None
        };
        out.push(SpongeSpec { face, thickness_cells, strength, pressure_pa, velocity_m_s, wave });
        normal.push(p.finish());
    }
    s.put("sponges", Value::Array(normal));
    Ok(out)
}

fn collision(s: &mut Section<'_>, lattice: LatticeKind) -> CaeResult<CollisionKind> {
    let v = s.raw("collision").cloned().unwrap_or_else(|| json!({"kind": "trt"}));
    let path = s.at("collision");
    let kind = kind_of(&v, &path, &COLLISIONS)?;
    let (c, normal) = match kind {
        "bgk" => {
            Section::new(&v, &path, &["kind"])?;
            (CollisionKind::Bgk, json!({"kind": "bgk"}))
        }
        "regularized" => {
            Section::new(&v, &path, &["kind"])?;
            (CollisionKind::Regularized, json!({"kind": "regularized"}))
        }
        "trt" => {
            let mut c = Section::new(&v, &path, &["kind", "magic"])?;
            c.put("kind", json!("trt"));
            let magic = c.number_or("magic", MAGIC_DEFAULT, positive, "positive")?;
            (CollisionKind::Trt { magic }, c.finish())
        }
        "mrt" => {
            let mut c = Section::new(&v, &path, &["kind", "bulk_rate", "odd_magic", "even_rate"])?;
            c.put("kind", json!("mrt"));
            let rate = |c: &mut Section<'_>, key: &str| -> CaeResult<Option<f64>> {
                if c.has(key) {
                    c.number(key, |x| x > 0.0 && x < 2.0, "in (0, 2)").map(Some)
                } else {
                    Ok(None)
                }
            };
            let bulk_rate = rate(&mut c, "bulk_rate")?;
            let odd_magic = c.number_or("odd_magic", MAGIC_DEFAULT, positive, "positive")?;
            let even_rate = rate(&mut c, "even_rate")?;
            (CollisionKind::Mrt { bulk_rate, odd_magic, even_rate }, c.finish())
        }
        _ => {
            if lattice != LatticeKind::D3Q27 {
                return refuse(format!(
                    "{path}: the cumulant collision needs the D3Q27 lattice (got {})",
                    lattice.name()
                ));
            }
            let mut c = Section::new(&v, &path, &["kind", "bulk_rate"])?;
            c.put("kind", json!("cumulant"));
            let bulk = c.number_or("bulk_rate", 1.0, |x| x > 0.0 && x < 2.0, "in (0, 2)")?;
            (CollisionKind::Cumulant { bulk_rate: Some(bulk) }, c.finish())
        }
    };
    s.put("collision", normal);
    Ok(c)
}

fn turbulence(s: &mut Section<'_>) -> CaeResult<Turbulence> {
    let v = s.raw("turbulence").cloned().unwrap_or_else(|| json!({"kind": "laminar"}));
    let path = s.at("turbulence");
    let kind = kind_of(&v, &path, &TURBULENCE)?;
    let (t, normal) = match kind {
        "laminar" => {
            Section::new(&v, &path, &["kind"])?;
            (Turbulence::Laminar, json!({"kind": "laminar"}))
        }
        "smagorinsky" => {
            let mut c = Section::new(&v, &path, &["kind", "constant", "norm_floor"])?;
            c.put("kind", json!("smagorinsky"));
            let constant = c.number_or("constant", 0.17, |x| x >= 0.0, "nonnegative")?;
            let norm_floor = c.number_or("norm_floor", 1e-10, positive, "positive")?;
            (Turbulence::Smagorinsky(Smagorinsky { constant, norm_floor }), c.finish())
        }
        _ => {
            let mut c = Section::new(&v, &path, &["kind", "constant", "denominator_floor"])?;
            c.put("kind", json!("wale"));
            let constant = c.number_or("constant", 0.5, |x| x >= 0.0, "nonnegative")?;
            let denominator_floor = c.number_or("denominator_floor", 1e-12, positive, "positive")?;
            (Turbulence::Wale(Wale { constant, denominator_floor }), c.finish())
        }
    };
    s.put("turbulence", normal);
    Ok(t)
}

fn coupling_law(s: &mut Section<'_>) -> CaeResult<CouplingLaw> {
    let v = s.raw("coupling_law").cloned().unwrap_or_else(|| json!({"kind": "psm_superposition"}));
    let path = s.at("coupling_law");
    if let Some(Value::String(k)) = v.get("kind")
        && let Some((_, reason)) = REFUSED_COUPLING_LAWS.iter().find(|(name, _)| name == k)
    {
        return refuse(format!("{path}.kind {k:?} is refused by the FSI provider: {reason}"));
    }
    let kind = kind_of(&v, &path, &COUPLING_LAWS)?;
    let (law, normal) = match kind {
        "psm_superposition" => {
            Section::new(&v, &path, &["kind"])?;
            (CouplingLaw::PsmSuperposition, json!({"kind": "psm_superposition"}))
        }
        "psm" => {
            Section::new(&v, &path, &["kind"])?;
            (CouplingLaw::Psm, json!({"kind": "psm"}))
        }
        _ => {
            let mut c = Section::new(&v, &path, &["kind", "drag_max_per_s", "drag_shape"])?;
            c.put("kind", json!("brinkman"));
            let drag_max_per_s = c.number("drag_max_per_s", positive, "positive")?;
            let drag_shape = c.number_or("drag_shape", 8.0, positive, "positive")?;
            (CouplingLaw::Brinkman { drag_max_per_s, drag_shape }, c.finish())
        }
    };
    s.put("coupling_law", normal);
    Ok(law)
}


#[allow(clippy::too_many_lines)]
pub fn parse(v: &Value) -> CaeResult<(FluidSpec, Value)> {
    let mut s = Section::new(v, "fluid", &FLUID_KEYS)?;
    let lattice = LatticeKind::parse(&s.choice_or("lattice", "D3Q19", &LATTICES)?)?;
    let shape = s.shape("shape", 4096)?;
    if shape.iter().product::<usize>() > 64_000_000 {
        return refuse("fluid.shape exceeds 6.4e7 lattice cells");
    }
    let spacing_m = s.number("spacing_m", positive, "positive")?;
    let origin_m = s.triple_or("origin_m", [0.0; 3])?;
    let periodic = s.flags3_or("periodic_axes", [false, false, shape[2] == 1])?;
    if lattice == LatticeKind::D2Q9 && !(shape[2] == 1 && periodic[2]) {
        return refuse("fluid: the D2Q9 lattice needs shape[2] = 1 with a periodic z axis");
    }
    let density_kg_m3 = s.number("density_kg_m3", positive, "positive")?;
    let kinematic_viscosity_m2_s = s.number("kinematic_viscosity_m2_s", positive, "positive")?;
    let collision = collision(&mut s, lattice)?;
    let turbulence = turbulence(&mut s)?;
    let coupling_law = coupling_law(&mut s)?;
    let mut solid_mask = walls(&mut s, shape, spacing_m, origin_m)?;
    match s.raw("solid_mask") {
        None | Some(Value::Null) => s.put("solid_mask", Value::Null),
        Some(m) => {
            let extra = mask_of(m, &s.at("solid_mask"), shape)?;
            for (a, b) in solid_mask.iter_mut().zip(&extra) {
                *a |= *b;
            }
            s.put("solid_mask", encode_mask(&extra, shape));
        }
    }
    let ports = ports(&mut s, shape, periodic, &solid_mask)?;
    let sponges = sponges(&mut s, periodic)?;
    let body_acceleration_m_s2 = s.triple_or("body_acceleration_m_s2", [0.0; 3])?;
    let initial_velocity_m_s = s.triple_or("initial_velocity_m_s", [0.0; 3])?;
    let initial_pressure_pa = s.number_or("initial_pressure_pa", 0.0, |_| true, "finite")?;
    let mach_limit = s.number_or("mach_limit", 0.15, |x| x > 0.0 && x < 1.0, "in (0, 1)")?;
    let lattice_velocity_limit =
        s.number_or("lattice_velocity_limit", 0.1, |x| x > 0.0 && x < 0.5, "in (0, 0.5)")?;
    let tau_min = s.number_or("tau_min", 1e-3, |x| x > 0.0 && x < 0.5, "in (0, 0.5)")?;
    let sampling = match s.choice_or("sampling", "end", &["end", "macro_mean"])?.as_str() {
        "end" => Sampling::End,
        _ => Sampling::MacroMean,
    };
    let inner = s.number_or(
        "inner_checkpoint_bytes",
        268_435_456.0,
        |x| x >= 1024.0 && x.fract() == 0.0,
        "an integer of at least 1024",
    )?;
    let regime = if s.has("regime") {
        let mut r = s.child("regime", &["velocity_m_s", "length_m", "reynolds_max"])?;
        let decl = RegimeDeclaration {
            velocity_m_s: r.number("velocity_m_s", positive, "positive")?,
            length_m: r.number("length_m", positive, "positive")?,
            reynolds_max: r.number_or("reynolds_max", 1000.0, positive, "positive")?,
        };
        s.put("regime", r.finish());
        Some(decl)
    } else {
        None
    };
    let mut symmetry_faces = Vec::new();
    let mut faces_normal = Vec::new();
    for (v, path) in s.list("symmetry_faces", 6)? {
        let name = v.as_str().unwrap_or_default().to_string();
        let face = Face::parse(&name).map_err(|_| {
            implexity_core::CaeError::contract(format!("{path} must name a lattice face (xmin … zmax)"))
        })?;
        if symmetry_faces.contains(&face) {
            return refuse(format!("{path}: the face {name} is listed twice"));
        }
        let axis = match face {
            Face::XMin | Face::XMax => 0,
            Face::YMin | Face::YMax => 1,
            Face::ZMin | Face::ZMax => 2,
        };
        if periodic[axis] {
            return refuse(format!("{path}: the face {name} lies on a periodic axis"));
        }
        if ports.iter().any(|p| p.face == face) {
            return refuse(format!("{path}: the face {name} carries a port and cannot be a symmetry face"));
        }
        symmetry_faces.push(face);
        faces_normal.push(json!(name));
    }
    if !faces_normal.is_empty() {
        s.put("symmetry_faces", Value::Array(faces_normal));
    }
    let entrained_inertia =
        match s.choice_or("entrained_inertia", "carried", &["carried", "compensated"])?.as_str() {
            "compensated" => EntrainedInertia::Compensated,
            _ => EntrainedInertia::Carried,
        };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let inner_checkpoint_bytes = inner as u64;
    let spec = FluidSpec {
        lattice,
        shape,
        spacing_m,
        origin_m,
        periodic,
        density_kg_m3,
        kinematic_viscosity_m2_s,
        collision,
        turbulence,
        coupling_law,
        solid_mask,
        ports,
        sponges,
        body_acceleration_m_s2,
        initial_velocity_m_s,
        initial_pressure_pa,
        mach_limit,
        lattice_velocity_limit,
        tau_min,
        sampling,
        inner_checkpoint_bytes,
        regime,
        symmetry_faces,
        entrained_inertia,
    };
    Ok((spec, s.finish()))
}

#[must_use]
pub fn cell_index(shape: [usize; 3], ijk: [usize; 3]) -> usize {
    index(shape, ijk)
}

#[must_use]
pub fn cell_coords(shape: [usize; 3], cell: usize) -> [usize; 3] {
    coords(shape, cell)
}
