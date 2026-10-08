// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_core::CaeResult;
use implexity_physics_lbm::moving::boundary::PortSpec;
use implexity_physics_lbm::moving::observables::{ObservableSpec, ProbeQuantity};
use implexity_physics_solid::soft_fsi::observables::SolidObservable;

use crate::json::{Section, kind_of, refuse};

pub const FLUID_KINDS: [&str; 7] =
    ["section_mass_flux", "section_flux", "probe", "port_power", "fluid_kinetic_energy", "solid_force", "section_open_area"];
pub const INTERFACE_KINDS: [&str; 1] = ["interface_power"];

pub const SOLID_KINDS: [&str; 6] = [
    "probe_displacement",
    "probe_separation",
    "plane_gap",
    "strain_energy",
    "solid_kinetic_energy",
    "stress_aggregate",
];

#[must_use]
pub fn unit_of(kind: &str) -> &'static str {
    match kind {
        "section_mass_flux" => "kg/s (actual final pull, before port reconstruction and collision)",
        "section_flux" => "m3/s (per metre depth in quasi-2-D lattices: m2/s times the layer thickness)",
        "probe" => "Pa, m/s or kg/m3",
        "port_power" | "fluid_kinetic_energy" | "strain_energy" | "solid_kinetic_energy" => "W or J",
        "solid_force" => "N",
        "interface_power" => "W (per metre depth in quasi-2-D lattices times the layer thickness)",
        "probe_displacement" | "probe_separation" | "plane_gap" => "m",
        "section_open_area" => "m2 (m per unit depth in quasi-2-D lattices)",
        "stress_aggregate" => "Pa",
        _ => "1",
    }
}

#[derive(Clone, Debug, Default)]
pub struct ObservableSpecs {
    pub solid: Vec<(String, SolidObservable)>,
    pub fluid: Vec<(String, ObservableSpec)>,
    pub interface: Vec<String>,
    pub kinds: Vec<(String, String)>,
}

impl ObservableSpecs {
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.solid
            .iter()
            .map(|(n, _)| n.clone())
            .chain(self.fluid.iter().map(|(n, _)| n.clone()))
            .chain(self.interface.iter().cloned())
            .collect()
    }
}

fn is_identifier(t: &str) -> bool {
    let mut c = t.chars();
    c.next().is_some_and(|x| x.is_ascii_alphabetic() || x == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
        && t.len() <= 64
}


#[allow(clippy::too_many_lines)]
pub fn parse(v: Option<&Value>, ports: &[PortSpec]) -> CaeResult<(ObservableSpecs, Value)> {
    let list = match v {
        None => return refuse("observables must list at least one observable"),
        Some(Value::Array(a)) if !a.is_empty() && a.len() <= 64 => a,
        Some(_) => return refuse("observables must be a list of 1..64 observables"),
    };
    let mut out = ObservableSpecs::default();
    let mut normal = Vec::new();
    let mut all_kinds: Vec<&str> = FLUID_KINDS.to_vec();
    all_kinds.extend(SOLID_KINDS);
    all_kinds.push("solid_probe");
    all_kinds.extend(INTERFACE_KINDS);
    for (i, o) in list.iter().enumerate() {
        let path = format!("observables[{i}]");
        let kind = kind_of(o, &path, &all_kinds)?;
        let keys: &[&str] = match kind {
            "section_flux" | "section_mass_flux" => &["name", "kind", "axis", "index"],
            "probe" => &["name", "kind", "point_m", "quantity", "component"],
            "port_power" => &["name", "kind", "port"],
            "solid_force" => &["name", "kind", "component"],
            "probe_displacement" | "solid_probe" => &["name", "kind", "point_m", "component"],
            "probe_separation" => &["name", "kind", "point_a_m", "point_b_m", "component"],
            "section_open_area" => &["name", "kind", "axis", "range", "beta"],
            "plane_gap" => &["name", "kind", "normal", "offset_m", "beta"],
            "stress_aggregate" => &["name", "kind", "p"],
            _ => &["name", "kind"],
        };
        let mut s = Section::new(o, &path, keys)?;
        let name = s.text_or("name", "", 64)?;
        if !is_identifier(&name) || out.kinds.iter().any(|(n, _)| *n == name) {
            return refuse(format!("{path}.name must be a unique identifier (letters, digits, underscore)"));
        }

        let kind = if kind == "solid_probe" { "probe_displacement" } else { kind };
        s.put("kind", json!(kind));
        match kind {
            "section_mass_flux" => {
                let axis = s.integer("axis", 0..=2)?;
                let index = s.integer("index", 0..=1_000_000)?;
                out.fluid.push((name.clone(), ObservableSpec::SectionMassFlux { axis, index }));
            }
            "section_flux" => {
                let axis = s.integer("axis", 0..=2)?;
                let index = s.integer("index", 0..=1_000_000)?;
                out.fluid.push((name.clone(), ObservableSpec::SectionFlux { axis, index }));
            }
            "probe" => {
                let point_m = s.triple("point_m")?;
                let q = s.choice_or("quantity", "pressure", &["pressure", "velocity", "density"])?;
                let quantity = match q.as_str() {
                    "pressure" => ProbeQuantity::Pressure,
                    "density" => ProbeQuantity::Density,
                    _ => ProbeQuantity::Velocity(s.integer("component", 0..=2)?),
                };
                if q != "velocity" && s.has("component") {
                    return refuse(format!("{path}.component applies to velocity probes only"));
                }
                out.fluid.push((name.clone(), ObservableSpec::Probe { point_m, quantity }));
            }
            "port_power" => {
                let id = s.text_or("port", "", 64)?;
                let port = ports.iter().position(|p| p.id == id).ok_or_else(|| {
                    implexity_core::CaeError::contract(format!("{path}.port names no fluid port"))
                })?;
                out.fluid.push((name.clone(), ObservableSpec::PortPower { port }));
            }
            "fluid_kinetic_energy" => out.fluid.push((name.clone(), ObservableSpec::KineticEnergy)),
            "section_open_area" => {
                let axis = s.integer("axis", 0..=2)?;
                let raw = s.raw("range").cloned().unwrap_or(Value::Null);
                let r = s.numbers_of("range", &raw, Some(2))?;
                let ok = r.iter().all(|v| v.fract() == 0.0 && *v >= 0.0 && *v <= 1e6) && r[0] <= r[1];
                if !ok {
                    return refuse(format!("{path}.range must be two ascending section indices"));
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let range = [r[0] as usize, r[1] as usize];
                s.put("range", json!(range));
                let beta = s.number_or("beta", 1.0, |x| x > 0.0, "positive")?;
                out.fluid.push((name.clone(), ObservableSpec::SectionOpenArea { axis, range, beta }));
            }
            "solid_force" => {
                let component = s.integer("component", 0..=2)?;
                out.fluid.push((name.clone(), ObservableSpec::SolidForce { component }));
            }
            "probe_displacement" => {
                let point_m = s.triple("point_m")?;
                let component = s.integer("component", 0..=2)?;
                out.solid.push((name.clone(), SolidObservable::ProbeDisplacement { point_m, component }));
            }
            "probe_separation" => {
                let point_a_m = s.triple("point_a_m")?;
                let point_b_m = s.triple("point_b_m")?;
                let component = s.integer("component", 0..=2)?;
                out.solid.push((
                    name.clone(),
                    SolidObservable::ProbeSeparation { point_a_m, point_b_m, component },
                ));
            }
            "plane_gap" => {
                let n = s.triple("normal")?;
                let norm = n.iter().map(|x| x * x).sum::<f64>().sqrt();
                if norm.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                    return refuse(format!("{path}.normal must be nonzero"));
                }
                let normal_v = n.map(|x| x / norm);
                s.put("normal", json!(normal_v));
                let offset_m = s.number("offset_m", |_| true, "finite")?;
                let beta = s.number("beta", |x| x > 0.0, "positive")?;
                out.solid
                    .push((name.clone(), SolidObservable::PlaneGap { normal: normal_v, offset_m, beta }));
            }
            "interface_power" => out.interface.push(name.clone()),
            "strain_energy" => out.solid.push((name.clone(), SolidObservable::StrainEnergy)),
            "solid_kinetic_energy" => out.solid.push((name.clone(), SolidObservable::KineticEnergy)),
            _ => {
                let p = s.number_or("p", 8.0, |x| x >= 2.0, "at least 2")?;
                out.solid.push((name.clone(), SolidObservable::StressAggregate { p }));
            }
        }
        out.kinds.push((name, kind.to_string()));
        normal.push(s.finish());
    }
    Ok((out, Value::Array(normal)))
}
