// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub mod coupling;
pub mod design;
pub mod fluid;
pub mod material_map;
pub mod observables;
pub mod removal;
pub mod solid;
pub mod time;

use serde_json::{Value, json};

use implexity_core::{CaeError, CaeResult};
use implexity_solve::dynamic_program::{self, DynamicProgram};

use crate::json::{Section, refuse};

pub use coupling::CouplingSpec;
pub use design::DesignSpec;
pub use fluid::FluidSpec;
pub use observables::ObservableSpecs;
pub use solid::SolidSpec;
pub use time::{PhaseSpec, TimeKind, TimeSpec};

pub const SCHEMA: &str = "implexity-fsi-dynamic-problem/1";

pub const TOP_KEYS: [&str; 12] = [
    "schema",
    "label",
    "provenance",
    "fluid",
    "solid",
    "design",
    "coupling",
    "time",
    "observables",
    "responses",
    "operating_points",
    "frames",
];

pub const DESIGN_VOLUME: &str = "design_volume_fraction";

pub const LATTICE_FRAME_FIELDS: [&str; 3] = ["speed", "pressure", "occupancy"];
pub const SOLID_FRAME_FIELDS: [&str; 6] = [
    "solid_displacement",
    "von_mises",
    "solid_strain",
    "solid_density",
    "removed_density",
    "modifier_intensity",
];

#[derive(Clone, Debug, PartialEq)]
pub struct OperatingPoint {
    pub label: String,
    pub weight: f64,
    pub overrides: Vec<(String, Value)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameSpec {
    pub count: usize,
    pub fields: Vec<String>,
    pub byte_limit: u64,
}

#[derive(Clone, Debug)]
pub struct FsiProblem {
    pub label: String,
    pub fluid: FluidSpec,
    pub solid: SolidSpec,
    pub design: DesignSpec,
    pub coupling: CouplingSpec,
    pub time: TimeSpec,
    pub observables: ObservableSpecs,
    pub program: DynamicProgram,
    pub operating_points: Vec<OperatingPoint>,
    pub frames: FrameSpec,
    normal: Value,
    identity: String,
}

impl FsiProblem {
    #[must_use]
    pub fn normal_form(&self) -> &Value {
        &self.normal
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[must_use]
    pub fn responses(&self) -> Vec<String> {
        self.program.responses()
    }


    pub fn at_operating_point(&self, k: usize) -> CaeResult<Self> {
        if self.operating_points.is_empty() {
            if k == 0 {
                return Ok(self.clone());
            }
            return refuse(format!("operating point {k} does not exist (the problem declares none)"));
        }
        let point = self
            .operating_points
            .get(k)
            .ok_or_else(|| CaeError::contract(format!("operating point {k} does not exist")))?;
        let mut raw = self.normal.clone();
        if let Some(m) = raw.as_object_mut() {
            m.insert("operating_points".into(), Value::Array(Vec::new()));
        }
        for (path, value) in &point.overrides {
            set_path(&mut raw, path, value.clone())?;
        }
        normalise(&raw)
    }
}

fn path_parts(path: &str) -> CaeResult<Vec<Result<String, usize>>> {
    let mut out = Vec::new();
    for seg in path.split('.') {
        let (key, rest) = seg.split_once('[').map_or((seg, ""), |(k, r)| (k, r));
        if key.is_empty() {
            return refuse(format!("operating-point path {path:?} has an empty key"));
        }
        out.push(Ok(key.to_string()));
        let mut rest = rest;
        while !rest.is_empty() {
            let (idx, tail) = rest.split_once(']').ok_or_else(|| {
                CaeError::contract(format!("operating-point path {path:?} has an unclosed index"))
            })?;
            let i: usize = idx.parse().map_err(|_| {
                CaeError::contract(format!("operating-point path {path:?} has a non-integer index"))
            })?;
            out.push(Err(i));
            rest = tail.strip_prefix('[').unwrap_or(tail);
            if !tail.is_empty() && !tail.starts_with('[') {
                return refuse(format!("operating-point path {path:?} is malformed"));
            }
        }
    }
    Ok(out)
}

fn set_path(root: &mut Value, path: &str, value: Value) -> CaeResult<()> {
    let parts = path_parts(path)?;
    let mut cur = root;
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        cur = match part {
            Ok(k) => {
                let m = cur.as_object_mut().ok_or_else(|| {
                    CaeError::contract(format!("operating-point path {path:?} leaves the document"))
                })?;
                if !m.contains_key(k) {
                    return refuse(format!("operating-point path {path:?} names no existing key"));
                }
                if last {
                    m.insert(k.clone(), value);
                    return Ok(());
                }
                m.get_mut(k).ok_or_else(|| CaeError::contract("internal: path"))?
            }
            Err(idx) => {
                let a = cur.as_array_mut().ok_or_else(|| {
                    CaeError::contract(format!("operating-point path {path:?} indexes a non-list"))
                })?;
                let slot = a.get_mut(*idx).ok_or_else(|| {
                    CaeError::contract(format!("operating-point path {path:?} index out of range"))
                })?;
                if last {
                    *slot = value;
                    return Ok(());
                }
                slot
            }
        };
    }
    refuse(format!("operating-point path {path:?} is empty"))
}

fn operating_points(s: &mut Section<'_>) -> CaeResult<Vec<OperatingPoint>> {
    let mut out = Vec::new();
    let mut normal = Vec::new();
    for (v, path) in s.list("operating_points", 32)? {
        let mut p = Section::new(v, &path, &["label", "weight", "overrides"])?;
        let label = p.text_or("label", &format!("point{}", out.len()), 64)?;
        let weight = p.number_or("weight", 1.0, |x| x >= 0.0, "nonnegative")?;
        let o = p
            .raw("overrides")
            .and_then(Value::as_object)
            .ok_or_else(|| CaeError::contract(format!("{path}.overrides must map dotted paths to values")))?;
        let overrides: Vec<(String, Value)> = o.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (k, _) in &overrides {
            if k.starts_with("operating_points") || k.starts_with("schema") {
                return refuse(format!("{path}.overrides may not change {k:?}"));
            }
            path_parts(k)?;
        }
        p.put("overrides", Value::Object(o.clone()));
        out.push(OperatingPoint { label, weight, overrides });
        normal.push(p.finish());
    }
    if !out.is_empty() && out.iter().map(|p| p.weight).sum::<f64>() <= 0.0 {
        return refuse("operating_points weights must have a positive sum");
    }
    s.put("operating_points", Value::Array(normal));
    Ok(out)
}

fn frames(s: &mut Section<'_>) -> CaeResult<FrameSpec> {
    let mut f = s.child_or_empty("frames", &["count", "fields", "byte_limit"])?;
    let count = f.integer_or("count", 0, 0..=16)?;
    let fields: Vec<String> = match f.raw("fields") {
        None => vec!["speed".into(), "occupancy".into(), "solid_displacement".into()],
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for x in a {
                let t = x.as_str().unwrap_or_default();
                if !(LATTICE_FRAME_FIELDS.contains(&t) || SOLID_FRAME_FIELDS.contains(&t))
                    || out.iter().any(|o| o == t)
                {
                    return refuse(format!(
                        "frames.fields must list distinct names of {}",
                        LATTICE_FRAME_FIELDS
                            .iter()
                            .chain(&SOLID_FRAME_FIELDS)
                            .copied()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                out.push(t.to_string());
            }
            out
        }
        Some(_) => return refuse("frames.fields must be a list of field names"),
    };
    f.put("fields", json!(fields));
    let limit = f.number_or(
        "byte_limit",
        67_108_864.0,
        |x| (1024.0..=1_073_741_824.0).contains(&x) && x.fract() == 0.0,
        "an integer in 1024..=2^30",
    )?;
    s.put("frames", f.finish());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(FrameSpec { count, fields, byte_limit: limit as u64 })
}

fn cross_check(f: &FluidSpec, sd: &SolidSpec, c: &CouplingSpec, t: &TimeSpec) -> CaeResult<()> {
    use implexity_physics_lbm::moving::field::LatticeKind;
    let planar_fluid = f.shape[2] == 1 && f.periodic[2];
    if f.entrained_inertia == implexity_physics_lbm::moving::field::EntrainedInertia::Compensated
        && sd.inertia_compensation > 0.0
    {
        return refuse(
            "fluid.entrained_inertia = compensated and solid.inertia_compensation > 0 correct the same carried-fluid inertia twice: choose one (the fluid-side correction needs no mass margin of the solid)",
        );
    }
    if f.lattice == LatticeKind::D2Q9 && !sd.plane_strain {
        return refuse(
            "the D2Q9 lattice needs a plane-strain solid (one voxel layer, solid.plane_strain = true)",
        );
    }
    if sd.plane_strain {
        if !planar_fluid {
            return refuse(
                "a plane-strain solid needs a planar lattice (fluid.shape[2] = 1 with a periodic z axis)",
            );
        }
        if (sd.grid.element_size_m - f.spacing_m).abs() > 1e-9 * f.spacing_m {
            return refuse(format!(
                "a plane-strain solid must have the lattice layer thickness: solid.reference_grid.element_size_m = {} but fluid.spacing_m = {}",
                sd.grid.element_size_m, f.spacing_m
            ));
        }
    } else if planar_fluid {
        return refuse("a planar lattice (one periodic z layer) needs a plane-strain solid");
    }

    let point_spacing = match c.points_per_cell_axis {
        Some(p) => f.spacing_m / p,
        None => sd.grid.element_size_m / c.points_per_axis as f64,
    };
    if point_spacing > c.kernel_width_cells as f64 * f.spacing_m * (1.0 + 1e-12) {
        return refuse(format!(
            "push-forward points are too sparse: element_size_m / points_per_axis = {point_spacing:e} m exceeds the kernel width {} m; raise coupling.pushforward.points_per_axis (or points_per_cell_axis) or width_cells",
            c.kernel_width_cells as f64 * f.spacing_m
        ));
    }
    if matches!(sd.scheme, implexity_physics_solid::soft::stepper::Scheme::Quasistatic)
        && matches!(c.mode, implexity_solve::multirate_coupling::CouplingMode::Loose { .. })
    {
        return refuse(
            "a quasistatic (massless) solid cannot be loosely coupled: the added-mass ratio is unbounded; use a strong coupling mode",
        );
    }
    let period = t.period_s;
    for p in &f.ports {
        let sig = p.signal;
        match &t.kind {
            TimeKind::PeriodicForced => {
                if sig.ramp_s > 0.0 {
                    return refuse(format!(
                        "port {:?}: a ramp makes the forcing non-periodic (periodic_forced)",
                        p.id
                    ));
                }
                let cycles = sig.frequency_hz * period;
                if sig.frequency_hz > 0.0 && (cycles - cycles.round()).abs() > 1e-9 * cycles.max(1.0) {
                    return refuse(format!(
                        "port {:?}: frequency {} Hz is not periodic with the declared period {period} s",
                        p.id, sig.frequency_hz
                    ));
                }
            }
            TimeKind::PeriodicAutonomous { .. } | TimeKind::SteadyStability { .. } => {
                if sig.frequency_hz > 0.0 || sig.ramp_s > 0.0 {
                    return refuse(format!(
                        "port {:?}: {} needs steady ports (no oscillation, no ramp); a periodically driven problem is periodic_forced",
                        p.id,
                        t.kind.name()
                    ));
                }
            }
            TimeKind::FixedHorizon { .. } => {}
        }
    }
    for (i, support) in sd.supports.iter().enumerate() {
        let Some(motion) = &support.motion else { continue };
        let (f, _) = motion.signal();
        match &t.kind {
            TimeKind::PeriodicForced | TimeKind::FixedHorizon { .. } => {
                let cycles = f * period;
                if (cycles - cycles.round()).abs() > 1e-9 * cycles.max(1.0) || cycles.round() < 1.0 {
                    return refuse(format!(
                        "solid.supports[{i}].motion: frequency {f} Hz is not periodic with the period {period} s (the solid loading repeats every steps_per_period macro steps)"
                    ));
                }
            }
            _ => {
                return refuse(format!(
                    "solid.supports[{i}].motion: prescribed motion is forcing; {} needs an unforced (autonomous) problem",
                    t.kind.name()
                ));
            }
        }
    }
    Ok(())
}

#[must_use]
pub fn design_terms(design: &DesignSpec) -> Vec<&'static str> {
    let mut out = vec![DESIGN_VOLUME];
    if design.removal.is_some() {
        out.extend(removal::REMOVAL_TERMS);
    }
    out
}


pub fn normalise(v: &Value) -> CaeResult<FsiProblem> {
    let mut s = Section::new(v, "problem", &TOP_KEYS)?;
    match s.raw("schema") {
        Some(Value::String(x)) if x == SCHEMA => s.put("schema", json!(SCHEMA)),
        _ => return refuse(format!("problem.schema must be {SCHEMA:?}")),
    }
    let label = s.text_or("label", "", 200)?;
    s.text_or("provenance", "", 4000)?;
    let (fluid, fnorm) =
        fluid::parse(s.raw("fluid").ok_or_else(|| CaeError::contract("problem.fluid is required"))?)?;
    s.put("fluid", fnorm);
    let (solid, snorm) = solid::parse(
        s.raw("solid").ok_or_else(|| CaeError::contract("problem.solid is required"))?,
        fluid.density_kg_m3,
    )?;
    s.put("solid", snorm);
    let (design, dnorm) = design::parse(&s.raw("design").cloned().unwrap_or_else(|| json!({})), &solid.grid)?;
    s.put("design", dnorm);
    let (coupling, cnorm) = coupling::parse(&s.raw("coupling").cloned().unwrap_or_else(|| json!({})))?;
    s.put("coupling", cnorm);
    let (time, tnorm) =
        time::parse(s.raw("time").ok_or_else(|| CaeError::contract("problem.time is required"))?)?;
    s.put("time", tnorm);
    cross_check(&fluid, &solid, &coupling, &time)?;
    let (observables, onorm) = observables::parse(s.raw("observables"), &fluid.ports)?;
    s.put("observables", onorm);
    let names = observables.names();
    if let TimeKind::PeriodicAutonomous { phase: PhaseSpec::Section { sample, .. }, .. } = &time.kind
        && !names.contains(sample)
    {
        return refuse(format!("time.phase_condition.sample {sample:?} names no observable"));
    }
    let rv = s.raw("responses").ok_or_else(|| CaeError::contract("problem.responses is required"))?;
    if solid.removal_modifier.is_some() && design.removal.is_none() {
        return refuse(
            "solid.removal_modifier needs a removal-only design (design.removal): the zone is defined by the material \
             removed from the reference occupancy",
        );
    }
    let program = dynamic_program::normalise(rv)
        .map_err(|e| e.context("responses"))?
        .bind(&names, &design_terms(&design))
        .map_err(|e| e.context("responses"))?;
    if !matches!(time.kind, TimeKind::SteadyStability { .. }) {
        program
            .admit(time.history_steps(), time.periodic(), time.autonomous())
            .map_err(|e| e.context("responses"))?;
    }
    s.put("responses", program.to_value());
    let operating_points = operating_points(&mut s)?;
    let frames = frames(&mut s)?;
    let normal = s.finish();
    let identity = implexity_core::json::canonical_sha256(&normal);
    let problem = FsiProblem {
        label,
        fluid,
        solid,
        design,
        coupling,
        time,
        observables,
        program,
        operating_points,
        frames,
        normal,
        identity,
    };
    for k in 0..problem.operating_points.len() {
        problem.at_operating_point(k).map_err(|e| e.context(&format!("operating_points[{k}]")))?;
    }
    Ok(problem)
}
