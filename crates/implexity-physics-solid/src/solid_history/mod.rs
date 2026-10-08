// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




mod kernel;
mod provider;
mod responses;

pub use kernel::{KernelProblem, SolidKernel, SolidMesh, mesh};
pub use provider::{NAME, NativeSolidHistoryProvider, SolidHistoryFactory, register_history_component};
pub use responses::{Observed, POLYMER_COLUMNS, ResponseEval, format_e3};

use serde_json::{Map, Value, json};

use implexity_core::CaeError;

use crate::components::selected_component;
use crate::material::MaterialLaw;
use crate::util::{contract, has_exact_keys, num, real_array};

pub const COORDS: [&str; 3] = ["model:control", "model:parameters", "model:spatial_fields"];
pub const RESPONSES: [&str; 8] = [
    "solid_plastic_strain",
    "solid_creep_strain",
    "solid_inelastic_heat_J",
    "solid_temperature_peak_K",
    "solid_mass_kg",
    "solid_elastic_energy_J",
    "solid_time_mean_compliance_J",
    "solid_kinetic_energy_J",
];
pub const UNITS: [&str; 8] = ["1", "1", "J", "K", "kg", "J", "J", "J"];
pub const HOST_RESPONSES: [&str; 6] = [
    "solid_plastic_strain",
    "solid_creep_strain",
    "solid_inelastic_heat_J",
    "solid_temperature_peak_K",
    "solid_mass_kg",
    "solid_elastic_energy_J",
];
pub const COMPONENT_KINDS: [(&str, &str); 3] =
    [("material", "material_properties"), ("plasticity", "plastic_evolution"), ("creep", "creep_evolution")];
pub const LIMITATIONS: [&str; 6] = [
    "Small-strain linear tetrahedra; two-material relaxed interpolation.",
    "Mechanical equilibrium is quasistatic unless structural_dynamics is authored; then Newmark average-acceleration inertia with consistent T4 mass and fixed supports. Neither form resolves wave propagation below the mesh and time resolution.",
    "Local AD sparse assembly; memory-budget admission, sparse-direct solves; no distributed solver.",
    "No coolant flow or phase kinetics; optional equilibrium latent storage retains solid mechanics. Prescribed-dose material history is not neutron transport, fracture or calibrated fatigue prediction.",
    "Reversible heat requires explicit constant_strain_thermoelastic_solid; other materials omit it. Finite-strain geometry changes are neglected.",
    "Optional reciprocal convection/radiation with finite thermal nodes, not coolant flow.",
];

fn finite_array(value: &Value, shape: &[usize], name: &str) -> Result<Vec<f64>, CaeError> {
    match real_array(value) {
        Some((s, v)) if s == shape && v.iter().all(|x| x.is_finite()) => Ok(v),
        Some((s, _)) => {
            contract(format!("{name}: expected finite array {}, received {}", py_shape(shape), py_shape(&s)))
        }
        None => contract(format!("{name}: expected rectangular numeric array {}", py_shape(shape))),
    }
}

#[must_use]
pub fn py_shape(shape: &[usize]) -> String {
    match shape.len() {
        1 => format!("({},)", shape[0]),
        _ => format!("({})", shape.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
    }
}

fn int_like(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => {
            let x = n.as_f64()?;
            #[allow(clippy::float_cmp, clippy::cast_possible_truncation)]
            (x.trunc() == x && x.is_finite()).then_some(x as i64)
        }
        _ => None,
    }
}

fn is_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}


#[allow(clippy::too_many_lines)]
pub fn normalise(problem: &Value) -> Result<Value, CaeError> {
    let Some(map) = problem.as_object() else { return contract("solid problem must be an object") };
    let mut p = Value::Object(map.clone());
    let required = [
        "grid",
        "times_s",
        "materials",
        "components",
        "temperature_initial_K",
        "displacement_bcs",
        "temperature_bcs",
        "tractions",
        "heat_fluxes",
        "volumetric_heat_W_m3",
        "regularisation",
        "numerics",
    ];
    let missing: Vec<&str> = required.iter().copied().filter(|k| !map.contains_key(*k)).collect();
    if !missing.is_empty() {
        return contract(format!("solid history missing authoring {}", crate::util::sorted_repr(missing)));
    }
    let optional = [
        "field_registration",
        "design_field_registrations",
        "name",
        "assembly",
        "thermal_reservoirs",
        "thermal_exchanges",
        "material_history",
        "inactive_phase_numerical_material",
        "material_history_numerical_extension",
        "viscoelasticity",
        "fatigue_observer",
        "nodal_forces_N",
        "structural_dynamics",
        "applicability_policy",
    ];
    let extra: Vec<&str> =
        map.keys().map(String::as_str).filter(|k| !required.contains(k) && !optional.contains(k)).collect();
    if !extra.is_empty() {
        return contract(format!("unsupported solid problem keys: {}", crate::util::sorted_repr(extra)));
    }
    let applicability = p.get("applicability_policy").and_then(Value::as_str).unwrap_or("enforce");
    if !matches!(applicability, "enforce" | "report_only")
        || p.get("applicability_policy").is_some_and(|v| !v.is_string())
    {
        return contract("applicability_policy must be enforce or report_only");
    }
    if applicability == "report_only"
        && p.get("inactive_phase_numerical_material").is_none_or(Value::is_null)
    {
        return contract("report_only requires an explicit supported numerical material continuation");
    }
    let grid_ok = p["grid"].as_array().is_some_and(|g| {
        g.len() == 3 && g.iter().all(|n| !n.is_boolean() && int_like(n).is_some_and(|i| i >= 1))
    });
    if !grid_ok {
        return contract("grid must contain three positive integers");
    }
    let grid: Vec<usize> = p["grid"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| usize::try_from(int_like(n).unwrap_or(1)).unwrap_or(1))
        .collect();
    p["grid"] = json!(grid);
    let times = real_array(&p["times_s"]).filter(|(s, _)| s.len() == 1).map(|(_, v)| v).unwrap_or_default();
    let nt = p["times_s"].as_array().map_or(0, Vec::len);
    if nt < 2
        || times.len() != nt
        || times.iter().any(|t| !t.is_finite())
        || times[0] != 0.0
        || times.windows(2).any(|w| w[1] - w[0] <= 0.0)
    {
        return contract("history times must start at 0 and strictly increase");
    }
    p["times_s"] = crate::util::floats(&times);
    let nn: usize = grid.iter().map(|g| g + 1).product();
    if p.get("nodal_forces_N").is_some_and(|v| !v.is_null()) {
        let Some((shape, loads)) = real_array(&p["nodal_forces_N"]) else {
            return contract("nodal_forces_N must be a rectangular numeric history");
        };
        if shape != [nt, nn, 3] || loads.iter().any(|v| !v.is_finite()) {
            return contract(
                "nodal_forces_N requires finite real [time, node, xyz] values in N, with C-order Cartesian grid nodes",
            );
        }
        if loads[..nn * 3].iter().any(|v| *v != 0.0) {
            return contract("stress-free initialisation requires zero initial nodal forces");
        }
        p["nodal_forces_N"] = crate::util::nested(&shape, &loads);
    }
    if p.get("fatigue_observer").is_some_and(|v| !v.is_null()) {
        let row = crate::fatigue::validate_fatigue_row(&p["fatigue_observer"])?.unwrap_or(Value::Null);
        let beyond =
            row["settings"]["cycles"].as_array().into_iter().flatten().any(|c| {
                c["end_index"].as_u64().is_some_and(|e| usize::try_from(e).unwrap_or(usize::MAX) >= nt)
            });
        if beyond {
            return contract("fatigue cycle exceeds authored solid history");
        }
        p["fatigue_observer"] = row;
    }
    if p["materials"].as_array().map_or(0, Vec::len) != 2 {
        return contract("current field interpolation requires two explicit material endpoints");
    }
    let material_name =
        p["components"].get("material").map_or_else(|| "None".into(), implexity_core::pyobj::py_str);
    let crate::components::SolidComponent::Material(law) =
        selected_component(&material_name, "material_properties")?
    else {
        return contract(format!(
            "{} is not an active material_properties component",
            implexity_core::py_repr::repr_str(&material_name)
        ));
    };
    if law.reversible_thermoelastic() {
        let present = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
        let selected = |k: &str| p["components"].get(k).is_some_and(|v| !v.is_null());
        if ["material_history", "viscoelasticity", "inactive_phase_numerical_material"]
            .iter()
            .any(|k| present(k))
            || selected("plasticity")
            || selected("creep")
        {
            return contract(
                "Helmholtz thermoelastic material cannot be combined with inelastic/history/continuation components",
            );
        }
    }
    let mut checked = Vec::new();
    let mut errors = Vec::new();
    for (i, m) in p["materials"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        match law.validate(m) {
            Ok(v) => checked.push(v),
            Err(e) => {
                let name = match m {
                    Value::Object(o) => {
                        o.get("name").map_or_else(|| "unnamed".to_string(), implexity_core::pyobj::py_str)
                    }
                    _ => "invalid object".to_string(),
                };
                errors.push(format!("material[{i}] {name}: {}", e.message()));
            }
        }
    }
    if !errors.is_empty() {
        return contract(errors.join("; "));
    }
    p["materials"] = Value::Array(checked.iter().map(|m| Value::Object(m.raw.clone())).collect());
    if map.contains_key("inactive_phase_numerical_material") {
        let policy = p["inactive_phase_numerical_material"].clone();
        if !law.supports_numerical_material() {
            return contract(
                "selected solid material does not support inactive-phase/endmember numerical continuation",
            );
        }
        let policies: Vec<Value> =
            checked.iter().map(|m| law.validate_numerical_material(m, &policy)).collect::<Result<_, _>>()?;
        if policies.iter().any(|r| r != &policies[0]) {
            return contract("solid endpoint materials disagree on the numerical continuation contract");
        }
        p["inactive_phase_numerical_material"] = policies[0].clone();
    }
    if p.get("material_history_numerical_extension").is_some_and(|v| !v.is_null())
        && p.get("material_history").is_none_or(Value::is_null)
    {
        return contract("material-history numerical extension requires a selected material history");
    }
    let t0 = &p["temperature_initial_K"];
    if checked.iter().any(|m| Some(m.t_ref) != t0.as_f64()) {
        return contract("initial stress-free temperature must equal both material reference temperatures");
    }
    let components_ok = p["components"]
        .as_object()
        .is_some_and(|c| c.len() == 3 && COMPONENT_KINDS.iter().all(|(k, _)| c.contains_key(*k)));
    if !components_ok {
        return contract(
            "explicit material, plasticity and creep selections required; null disables an evolution law",
        );
    }
    for (key, kind) in COMPONENT_KINDS {
        let value = &p["components"][key];
        if value.is_null() && key != "material" {
            continue;
        }
        selected_component(&implexity_core::pyobj::py_str(value), kind)?;
    }
    let plasticity_name = p["components"]["plasticity"].as_str().map(str::to_string);
    law.validate_bindings(plasticity_name.as_deref())?;
    let t_initial = t0.as_f64().unwrap_or(f64::NAN);
    for family in ["displacement_bcs", "temperature_bcs", "tractions", "heat_fluxes"] {
        let Some(rows) = p[family].as_array().cloned() else {
            return contract(format!("{family} must be an array"));
        };
        for bc in &rows {
            let mut base = vec!["axis", "side", "values"];
            if family == "displacement_bcs" {
                base.push("component");
            }
            if !has_exact_keys(bc, &base) {
                return contract(format!("{family}: expected keys {}", crate::util::sorted_repr(base)));
            }
            let axis_ok = is_int(&bc["axis"]) && bc["axis"].as_i64().is_some_and(|a| (0..=2).contains(&a));
            if !axis_ok || !(bc["side"] == json!("lo") || bc["side"] == json!("hi")) {
                return contract("face must specify axis 0/1/2 and side lo/hi");
            }
            if family == "displacement_bcs"
                && !(is_int(&bc["component"])
                    && bc["component"].as_i64().is_some_and(|a| (0..=2).contains(&a)))
            {
                return contract("displacement component must be 0/1/2");
            }
            let shape: Vec<usize> = if family == "tractions" { vec![nt, 3] } else { vec![nt] };
            let v = finite_array(&bc["values"], &shape, family)?;
            let first = &v[..shape.iter().skip(1).product::<usize>()];
            if family != "temperature_bcs" && first.iter().any(|x| *x != 0.0) {
                return contract("this stress-free initialisation requires zero initial loads/displacements");
            }
            #[allow(clippy::float_cmp)]
            if family == "temperature_bcs" && (v.iter().any(|x| *x <= 0.0) || v[0] != t_initial) {
                return contract("temperature BC must be positive and start at the initial temperature");
            }
        }
    }
    let heat = finite_array(&p["volumetric_heat_W_m3"], &[nt], "volumetric heat")?;
    if heat[0] != 0.0 {
        return contract("initial volumetric heat must be zero");
    }
    let r = &p["regularisation"];
    if !has_exact_keys(r, &["conductivity_floor_W_mK", "stiffness_floor", "topology_penalty"]) {
        return contract("explicit stiffness/conductivity regularisation and topology exponent required");
    }
    let rv = |k: &str| num(&r[k]);
    let valid =
        ["stiffness_floor", "conductivity_floor_W_mK", "topology_penalty"].iter().all(|k| rv(k).is_some())
            && rv("stiffness_floor").is_some_and(|s| 0.0 < s && s < 1.0)
            && rv("conductivity_floor_W_mK").is_some_and(|s| s > 0.0)
            && rv("topology_penalty").is_some_and(|s| s >= 1.0);
    if !valid {
        return contract("invalid void regularisation");
    }
    let numerics = &p["numerics"];
    let numeric_keys = [
        "conductivity_scale_W_mK",
        "length_scale_m",
        "max_iterations",
        "max_small_strain",
        "strain_scale",
        "stress_scale_Pa",
        "temperature_scale_K",
        "tolerance",
    ];
    if !has_exact_keys(numerics, &numeric_keys) {
        return contract("explicit numerical scaling and validity settings required");
    }
    if numeric_keys.iter().any(|k| num(&numerics[*k]).is_none_or(|v| v <= 0.0)) {
        return contract("all numerical scaling/limits must be positive finite");
    }
    if int_like(&numerics["max_iterations"]).is_none() {
        return contract("Newton iteration limit must be integer");
    }
    if numerics["max_small_strain"].as_f64().unwrap_or(0.0) > 0.1 {
        return contract("small-strain formulation cannot authorize strains above 0.1");
    }
    if p.get("structural_dynamics").is_some_and(|v| !v.is_null()) {
        let block = crate::structural_inertia::validate(&p["structural_dynamics"], &p)?;
        p["structural_dynamics"] = block;
    } else if let Some(m) = p.as_object_mut() {
        m.remove("structural_dynamics");
    }
    if map.contains_key("material_history") {
        let declared = crate::history::declaration(&p["material_history"], &p)?;
        p["material_history"] = declared.unwrap_or(Value::Null);
    }
    let mh = crate::history::bind(p.get("material_history").unwrap_or(&Value::Null), &p)?;
    let plastic = match p["components"]["plasticity"].as_str() {
        Some(name) => match selected_component(name, "plastic_evolution")? {
            crate::components::SolidComponent::Plastic(l) => Some(l),
            _ => None,
        },
        None => None,
    };
    let creep = match p["components"]["creep"].as_str() {
        Some(name) => match selected_component(name, "creep_evolution")? {
            crate::components::SolidComponent::Creep(l) => Some(l),
            _ => None,
        },
        None => None,
    };
    let mut viscoelastic_size = None;
    if p.get("viscoelasticity").is_some_and(|v| !v.is_null()) {
        let polymer = crate::polymer::bind_viscoelastic(&p["viscoelasticity"], &p)?;
        if let Some(polymer) = polymer {
            viscoelastic_size = Some(polymer.size());
            p["viscoelasticity"] =
                json!({"component": p["viscoelasticity"]["component"], "settings": polymer.settings});
        }
    }
    let layout = crate::inelastic::layout_for(plastic, creep, viscoelastic_size, &checked)?;
    let ni = layout.material_start() + mh.as_ref().map_or(0, |m| m.size);
    let nl = 16 + ni;
    let assembly = match p.get("assembly").filter(|v| crate::history::ageing::truthy(v)) {
        Some(a) => a.clone(),
        None => json!({"batch_size": 64, "max_estimated_bytes": 1_073_741_824_i64}),
    };
    let assembly_ok = has_exact_keys(&assembly, &["batch_size", "max_estimated_bytes"])
        && ["batch_size", "max_estimated_bytes"]
            .iter()
            .all(|k| is_int(&assembly[*k]) && assembly[*k].as_i64().is_some_and(|v| v >= 1));
    if !assembly_ok {
        return contract("assembly requires positive integer batch_size and max_estimated_bytes");
    }
    let batch = assembly["batch_size"].as_u64().unwrap_or(0);
    if batch > 4096 {
        return contract("local assembly batch_size must not exceed 4096");
    }
    let nc: u128 = grid.iter().map(|g| *g as u128).product();
    let nn = nn as u128;
    let (nt128, nl128, ni128, batch128) = (nt as u128, nl as u128, ni as u128, u128::from(batch));
    let mut nz = 4 * nn + 6 * ni128 * nc;
    let mut dynamic = 0u128;
    if p.get("structural_dynamics").is_some() {
        nz += 6 * nn;
        dynamic = 6 * nc * 12 * 40 * 3 * 8 + batch128 * 12 * 40 * 8 * 8;
    }
    let estimate =
        6 * nc * nl128 * nl128 * 3 * 24 + 8 * nz * nt128 * 8 + batch128 * nl128 * nl128 * 8 * 8 + dynamic;
    let budget = u128::try_from(assembly["max_estimated_bytes"].as_i64().unwrap_or(0)).unwrap_or(0);
    if estimate > budget {
        return contract(format!(
            "estimated assembly/history workspace {estimate} bytes exceeds authored resource budget {budget}; increase budget or change discretisation"
        ));
    }
    p["assembly"] = assembly;
    crate::solid_exchange::validate_exchange(&mut p)?;
    Ok(p)
}

#[must_use]
pub fn solid_boundary_editor_schema(family: &str) -> Value {
    let unit = match family {
        "displacement_bcs" => "m",
        "temperature_bcs" => "K",
        "tractions" => "Pa",
        _ => "W/m²",
    };
    let scalar = json!({"type": "number", "unit": unit});
    let mut values = json!({"title": "Values at each history time", "type": "array", "items": scalar});
    if family == "tractions" {
        let prefix: Vec<Value> = ["X", "Y", "Z"]
            .iter()
            .map(|axis| {
                let mut s = scalar.clone();
                s["title"] = json!(axis);
                s
            })
            .collect();
        values["items"] = json!({"type": "array", "minItems": 3, "maxItems": 3, "prefixItems": prefix});
    }
    let mut properties = json!({"axis": {"title": "Face axis (0=X, 1=Y, 2=Z)", "type": "integer", "enum": [0, 1, 2]},
        "side": {"title": "Face side (lo=minimum, hi=maximum)", "enum": ["lo", "hi"]}, "values": values});
    if family == "displacement_bcs" {
        properties["component"] =
            json!({"title": "Displacement direction (0=X, 1=Y, 2=Z)", "type": "integer", "enum": [0, 1, 2]});
    }
    let mut starter = json!({"axis": 0, "side": "lo", "values": match family {
        "tractions" => json!([[0.0, 0.0, 0.0], [0.0, 0.0, 0.0]]),
        "temperature_bcs" => json!([300.0, 300.0]),
        _ => json!([0.0, 0.0]),
    }});
    if family == "displacement_bcs" {
        starter["component"] = json!(0);
    }
    json!({"type": "object", "properties": properties, "default": starter})
}

#[must_use]
pub fn solid_material_editor_schema() -> Value {
    let labels: [(&str, &str, &str); 18] = [
        ("E", "Young modulus", "Pa"),
        ("nu", "Poisson ratio", "1"),
        ("yield_stress", "Initial yield stress", "Pa"),
        ("H_iso", "Isotropic hardening modulus", "Pa"),
        ("H_kin", "Kinematic hardening modulus", "Pa"),
        ("alpha", "Thermal expansion coefficient", "1/K"),
        ("k", "Thermal conductivity", "W/(m K)"),
        ("cp", "Specific heat capacity", "J/(kg K)"),
        ("density", "Density", "kg/m³"),
        ("creep_rate_ref", "Reference creep strain rate", "1/s"),
        ("creep_stress_ref", "Reference creep stress", "Pa"),
        ("creep_exponent", "Creep stress exponent", "1"),
        ("creep_activation_J_mol", "Creep activation energy", "J/mol"),
        ("creep_T_ref", "Reference creep temperature", "K"),
        ("taylor_quinney", "Inelastic work converted to heat", "1"),
        ("T_ref", "Material reference temperature", "K"),
        ("T_min", "Minimum valid temperature", "K"),
        ("T_max", "Maximum valid temperature", "K"),
    ];
    let mut properties = Map::new();
    for (key, title, unit) in labels {
        properties.insert(key.into(), json!({"title": title, "unit": unit, "type": "number"}));
    }
    let title = |k: &str| labels.iter().find(|(key, _, _)| *key == k).map_or("", |(_, t, _)| t);
    let slopes: Map<String, Value> =
        [("E", "Pa/K"), ("yield_stress", "Pa/K"), ("alpha", "1/K²"), ("k", "W/(m K²)"), ("cp", "J/(kg K²)")]
            .iter()
            .map(|(k, u)| {
                (
                    (*k).to_string(),
                    json!({"title": format!("{} slope", title(k)), "unit": u, "type": "number"}),
                )
            })
            .collect();
    properties.insert("name".into(), json!({"title": "Material name"}));
    properties.insert("provenance".into(), json!({"title": "Data source and calibration"}));
    properties.insert("temperature_slopes".into(), json!({"title": "Linear temperature slopes",
        "description": "Absolute coefficient change per kelvin, not a relative multiplier. Values are referenced to the material reference temperature.",
        "properties": slopes}));
    json!({"type": "object", "properties": properties})
}

#[must_use]
pub fn solid_editor_properties(dynamics: bool) -> Map<String, Value> {
    let mut p = crate::util::obj(json!({
        "applicability_policy": {"type": "string", "enum": ["enforce", "report_only"], "default": "enforce"},
        "name": {"title": "Study name and scope"},
        "grid": {"title": "Solid cells X/Y/Z", "description": "Must match the authored topology and material arrays. The starter is one cell, not automatically the displayed CAD. Physical spacing is supplied through the model:parameters design coordinate in mm.", "items": {"type": "integer", "minimum": 1}},
        "times_s": {"title": "History times", "unit": "s", "description": "Start at zero, then strictly increase. Every boundary/load history needs one entry per time."},
        "materials": {"title": "Explicit material endpoints", "description": "Synthetic starter coefficients, not calibrated data. Replace both endpoints for engineering use.", "minItems": 2, "maxItems": 2, "items": solid_material_editor_schema()},
        "temperature_initial_K": {"title": "Stress-free initial temperature", "unit": "K", "exclusiveMinimum": 0},
        "displacement_bcs": {"title": "Displacement boundary histories", "description": "Displacements in metres, one per time; initial value must be zero.", "items": solid_boundary_editor_schema("displacement_bcs")},
        "temperature_bcs": {"title": "Temperature boundary histories", "description": "Temperatures in kelvin, one per time.", "items": solid_boundary_editor_schema("temperature_bcs")},
        "tractions": {"title": "Applied face traction histories", "description": "Applied force per area in Pa, global xyz components at each time. Initial traction must be zero.", "items": solid_boundary_editor_schema("tractions")},
        "heat_fluxes": {"title": "Applied face heat-flux histories", "description": "Heat flux entering the solid through the face in W/m², one value per time. Positive values add heat to the solid (incident load, same sign as volumetric_heat_W_m3); negative values remove heat. Initial flux must be zero. Use thermal_exchanges for a temperature-dependent (Robin) exchange.", "items": solid_boundary_editor_schema("heat_fluxes")},
        "volumetric_heat_W_m3": {"title": "Volumetric heat history", "unit": "W/m³"},
        "nodal_forces_N": {"title": "Applied nodal force history", "format": "json",
            "description": "Optional [time][node][xyz] forces in N. C-order Cartesian nodes; zero initial load. Additive to face tractions. These are prescribed dead loads, not live CFD feedback."}}));
    if dynamics {
        p.insert("structural_dynamics".into(), crate::structural_inertia::editor_schema());
    }
    p
}

#[must_use]
pub fn merge_editor_schema(base: &Value, overlay: &Value) -> Value {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            let mut out = b.clone();
            for (k, v) in o {
                let merged = match out.get(k) {
                    Some(existing @ Value::Object(_)) if v.is_object() => merge_editor_schema(existing, v),
                    _ => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        _ => overlay.clone(),
    }
}

fn merge_patch(base: &Value, patch: &Value) -> Value {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let mut out = b.clone();
            for (k, v) in p {
                out.insert(k.clone(), merge_patch(b.get(k).unwrap_or(&Value::Null), v));
            }
            Value::Object(out)
        }
        _ => patch.clone(),
    }
}

#[must_use]
pub fn solid_study_templates(solid: &Value, prefix: &[&str]) -> Vec<Value> {
    if !solid.is_object() {
        return Vec::new();
    }
    let nest = |value: Value| prefix.iter().rev().fold(value, |v, k| json!({*k: v}));
    let mut rows = Vec::new();
    for (_, component) in crate::components::study_template_components() {
        for row in component.solid_study_templates(solid).unwrap_or_default() {
            if normalise(&merge_patch(solid, &row["problem_patch"])).is_err() {
                continue;
            }
            let mut schema_patch = row.get("editor_schema_patch").cloned().filter(|v| !v.is_null());
            for key in prefix.iter().rev() {
                schema_patch = schema_patch.map(|s| json!({"properties": {*key: s}}));
            }
            let mut out = Map::new();
            out.insert("schema".into(), json!("implexity-provider-study-template/1"));
            for (k, v) in row.as_object().into_iter().flatten() {
                if !["problem_patch", "problem_requirements", "editor_schema_patch"].contains(&k.as_str()) {
                    out.insert(k.clone(), v.clone());
                }
            }
            out.insert("problem_patch".into(), nest(row["problem_patch"].clone()));
            let requirements: Vec<Value> = row["problem_requirements"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|req| {
                    let mut r = req.clone();
                    let mut path: Vec<Value> = prefix.iter().map(|p| json!(p)).collect();
                    path.extend(req["path"].as_array().cloned().unwrap_or_default());
                    r["path"] = Value::Array(path);
                    r
                })
                .collect();
            out.insert("problem_requirements".into(), Value::Array(requirements));
            if let Some(s) = schema_patch {
                out.insert("editor_schema_patch".into(), s);
            }
            rows.push(Value::Object(out));
        }
    }
    rows
}

#[must_use]
pub fn solid_history_starter() -> Value {
    let material = json!({"name": "Synthetic solid A", "provenance": "Illustrative coefficients, not a calibrated material",
        "E": 1e9, "nu": 0.3, "yield_stress": 1e7, "H_iso": 0.0, "H_kin": 0.0, "alpha": 0.0, "k": 1.0, "cp": 1000.0, "density": 1000.0,
        "creep_rate_ref": 0.0, "creep_stress_ref": 1e6, "creep_exponent": 1.0, "creep_activation_J_mol": 0.0, "creep_T_ref": 300.0,
        "taylor_quinney": 0.0, "T_ref": 300.0, "T_min": 250.0, "T_max": 400.0,
        "temperature_slopes": {"E": 0.0, "yield_stress": 0.0, "alpha": 0.0, "k": 0.0, "cp": 0.0}});
    let mut other = material.clone();
    other["name"] = json!("Synthetic solid B");
    other["E"] = json!(2e9);
    other["k"] = json!(2.0);
    other["density"] = json!(2000.0);
    json!({"name": "Synthetic one-cell solid, match grid and replace material data before use",
        "grid": [1, 1, 1], "times_s": [0.0, 1.0], "materials": [material, other],
        "components": {"material": "temperature_linear_solid", "plasticity": null, "creep": null},
        "temperature_initial_K": 300.0,
        "displacement_bcs": (0..3).map(|a| json!({"axis": 0, "side": "lo", "component": a, "values": [0.0, 0.0]})).collect::<Vec<_>>(),
        "temperature_bcs": [{"axis": 0, "side": "lo", "values": [300.0, 300.0]}],
        "tractions": [{"axis": 0, "side": "hi", "values": [[0.0, 0.0, 0.0], [1e4, 0.0, 0.0]]}],
        "nodal_forces_N": null, "heat_fluxes": [], "volumetric_heat_W_m3": [0.0, 0.0],
        "regularisation": {"stiffness_floor": 0.001, "conductivity_floor_W_mK": 0.01, "topology_penalty": 3.0},
        "numerics": {"tolerance": 1e-9, "max_iterations": 20, "strain_scale": 0.001, "stress_scale_Pa": 1e6,
            "temperature_scale_K": 1.0, "length_scale_m": 0.01, "conductivity_scale_W_mK": 1.0, "max_small_strain": 0.05},
        "assembly": {"batch_size": 64, "max_estimated_bytes": 268_435_456}})
}

#[must_use]
pub fn provider_editor_schema(problem: &Value) -> Value {
    let mut schema = crate::history::editor_schema(problem);
    let phase = problem
        .get("components")
        .and_then(|c| c.get("material"))
        .is_some_and(|m| m == "phase_transition_caloric_solid");
    if problem.is_object() && phase {
        let fields = json!({
            "transition_temperature_K": {"title": "Equilibrium transition temperature", "type": "number", "units": "K"},
            "transition_width_K": {"title": "Smooth transition width", "type": "number", "units": "K", "exclusiveMinimum": 0},
            "latent_heat_J_kg": {"title": "Latent heat per unit mass", "type": "number", "units": "J/kg", "minimum": 0}});
        let mut endpoints = Vec::new();
        for material in problem.get("materials").and_then(Value::as_array).into_iter().flatten() {
            let mut endpoint = fields.clone();
            if material.is_object() {
                for (source, target) in [("T_min", "exclusiveMinimum"), ("T_max", "exclusiveMaximum")] {
                    if let Some(v) = material.get(source) {
                        endpoint["transition_temperature_K"][target] = v.clone();
                    }
                }
            }
            endpoints.push(json!({"properties": endpoint}));
        }
        if !schema.is_object() || schema.as_object().is_some_and(Map::is_empty) {
            schema = json!({});
        }
        if schema.get("properties").is_none() {
            schema["properties"] = json!({});
        }
        schema["properties"]["materials"] = json!({"title": "Solid material endpoints", "type": "array",
            "minItems": 2, "maxItems": 2, "items": {"properties": fields}, "prefixItems": endpoints});
    }
    schema
}

#[must_use]
pub fn component_slots() -> Map<String, Value> {
    crate::util::obj(json!({
        "material": {"component_kind": "material_properties", "required": true, "integration": "current_state_and_all_history_adjoint"},
        "plasticity": {"component_kind": "plastic_evolution", "required": false, "integration": "current_state_and_all_history_adjoint"},
        "creep": {"component_kind": "creep_evolution", "required": false, "integration": "current_state_and_all_history_adjoint"},
        "material_history": {"component_kind": "material_state_evolution", "required": false, "integration": "current_state_residual_energy_and_all_history_adjoint"},
        "viscoelasticity": {"component_kind": "viscoelastic_solid", "required": false, "integration": "native_branch_state_stress_energy_and_all_history_adjoint"},
        "fatigue_observer": {"component_kind": "fatigue_history_observer", "required": false, "integration": "postprocess_solved_history_no_gradient_or_stiffness_feedback"},
        "thermal_exchanges": {"component_kind": "thermal_exchange", "required": false, "integration": "reciprocal_surface_power_and_all_history_adjoint"}}))
}

#[must_use]
pub fn authoring_contract() -> Map<String, Value> {
    crate::util::obj(json!({"schema": "implexity-native-solid-authoring/1",
        "optional_nodal_forces_N": {"shape": "[number of times, product(grid+1), 3]",
            "ordering": "C-order Cartesian nodes, xyz components; positions node_index*spacing_mm*1e-3; local volume origin is zero",
            "convention": "Applied force on the solid in N, not traction in Pa; initial values must be zero",
            "scope": "Prescribed dead loads attached to node labels, additive to face tractions. Imported CFD loads are frozen; no flow re-solve or upstream flow derivative."},
        "optional_thermoelastic_material": {"component": "constant_strain_thermoelastic_solid",
            "capacity": "constant_strain_heat_capacity_J_m3_K replaces cp; J/(m3 K), not mass-specific",
            "coefficients": "all temperature slopes zero; remaining explicit native material keys unchanged",
            "discretisation": "backward-Euler entropy storage; numerical energy defect is not physical heat",
            "scope": "no simultaneous plasticity, creep, viscoelasticity, material history or numerical continuation"},
        "optional_viscoelasticity": {"component": "native_maxwell_polymer", "settings_schema": "implexity-native-maxwell-polymer/1",
            "scope": "single authored spectrum; optional explicit environmental-ageing stiffness/energy coupling; no simultaneous plasticity or creep",
            "integration": "coupled branch states, stress and heat; parameter data supplied by user"}}))
}


pub(crate) fn refuse_chaboche_continuation(law: MaterialLaw, numerical: bool) -> Result<(), CaeError> {
    if numerical && law == MaterialLaw::ChabocheTable {
        return contract(
            "inactive-phase numerical continuation does not extend Chaboche kinematic-hardening branches",
        );
    }
    Ok(())
}
