// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;

use super::{HistoryComponent, HistoryEnergy, HistoryLaw, MaterialHistoryBinding, selected};
use crate::mandel::Mandel;
use crate::util::{contract, has_exact_keys, text};

pub const CONTRACT: &str = "relative_property_factors_zero_preserving_creep/1";
pub const COMBINATION: &str = "independent_relative_property_factors";
pub const ENERGY: &str = "sum_of_independent_disjoint_storage_reservoirs";
pub const MAX_CHILDREN: usize = 8;
pub const LIMITATIONS: [&str; 5] = [
    "Explicit independent local histories, not automatically inferred kinetic interactions.",
    "Conductivity, yield and creep relative factors multiply; joint calibration is required.",
    "Storage reservoirs must be disjoint; source duplication is not automatically inferred.",
    "No evolving elastic/caloric/eigenstrain law, no released-species transport.",
    "No nested composites, implicit member activation or external transport-source routing.",
];

#[must_use]
pub fn authoring_contract() -> Value {
    json!({"schema": "implexity-component-authoring/1", "selection": "solid.material_history",
        "required_settings": ["name", "provenance", "members", "property_combination", "energy_convention"],
        "member_fields": ["id", "component", "settings"],
        "property_combination": COMBINATION, "energy_convention": ENERGY,
        "calibrated_material_data_supplied": false})
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub id: String,
    pub component: HistoryComponent,
    pub law: HistoryLaw,
    pub state: std::ops::Range<usize>,
    pub forcing: std::ops::Range<usize>,
}


pub fn editor_schema(settings: &Value, context: &Value) -> Result<Value, CaeError> {
    let mut members = Vec::new();
    for row in settings.get("members").and_then(Value::as_array).into_iter().flatten() {
        let component = selected(row["component"].as_str().unwrap_or_default())?;
        members.push(json!({"title": row["id"], "properties": {
            "id": {"const": row["id"]}, "component": {"const": row["component"]},
            "settings": component.editor_schema(&row["settings"], context)?}}));
    }
    Ok(json!({"properties": {"members": {"title": "Simultaneously solved material histories",
        "type": "array", "minItems": 2, "maxItems": MAX_CHILDREN, "prefixItems": members},
        "property_combination": {"enum": [COMBINATION]}, "energy_convention": {"enum": [ENERGY]},
        "provenance": {"title": "Joint calibration and independent-energy justification"}}}))
}

fn snake_id(id: &str) -> bool {

    let mut chars = id.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
        return false;
    }
    let mut previous_underscore = false;
    for c in chars {
        if c == '_' {
            if previous_underscore {
                return false;
            }
            previous_underscore = true;
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            previous_underscore = false;
        } else {
            return false;
        }
    }
    !previous_underscore
}


pub fn validate(s: &Value, context: &Value) -> Result<Value, CaeError> {
    let required = ["energy_convention", "members", "name", "property_combination", "provenance"];
    let keys_ok = s.as_object().is_some_and(|m| {
        let keys: Vec<&str> = m.keys().map(String::as_str).filter(|k| *k != "layout").collect();
        keys.len() == required.len() && required.iter().all(|k| keys.contains(k))
    });
    if !keys_ok {
        return contract(format!(
            "composite material history requires {}",
            crate::util::sorted_repr(required)
        ));
    }
    if !(text(&s["name"]) && text(&s["provenance"])) {
        return contract("composite name and joint calibration provenance required");
    }
    if s["property_combination"] != json!(COMBINATION) || s["energy_convention"] != json!(ENERGY) {
        return contract("explicit independent property/storage composition required");
    }
    if context.get("viscoelasticity").is_some_and(|v| !v.is_null()) {
        return contract(
            "composite material history does not implement Maxwell storage-softening interactions",
        );
    }
    let Some(members) = s["members"].as_array().filter(|m| (2..=MAX_CHILDREN).contains(&m.len())) else {
        return contract(format!("composite requires 2..{MAX_CHILDREN} flat members"));
    };
    let mut result = s.clone();
    let mut layout = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (k, row) in members.iter().enumerate() {
        if !has_exact_keys(row, &["component", "id", "settings"]) {
            return contract("composite member requires id, component, settings");
        }
        let ident = row["id"].as_str().unwrap_or_default();
        if !row["id"].is_string()
            || ident.chars().count() > 64
            || !snake_id(ident)
            || names.iter().any(|n| n == ident)
        {
            return contract("unique snake_case member IDs required");
        }
        names.push(ident.to_string());
        let component_name = row["component"].as_str().filter(|c| *c != "composite_material_history");
        let Some(component_name) = component_name else {
            return contract("nested/invalid material-history composition is unsupported");
        };
        let component = selected(component_name)?;
        if component.composition_contract() != Some(CONTRACT) {
            return contract("member has not declared the required property-composition contract");
        }
        let config = component.validate(&row["settings"], context)?;
        let child = MaterialHistoryBinding::new(component_name, component, config, context)?;
        result["members"][k]["settings"] = child.settings.clone();
        layout.push(json!({"id": ident, "state_size": child.size, "forcing_size": child.forcing_shape[2]}));
    }
    if let Some(declared) = s.get("layout")
        && declared != &Value::Array(layout.clone())
    {
        return contract("material-history layout disagrees with validated child contracts");
    }
    if layout.iter().map(|r| r["state_size"].as_u64().unwrap_or(0)).sum::<u64>() > 64 {
        return contract("material-history state budget exceeds 64 components");
    }
    result["layout"] = Value::Array(layout);
    Ok(result)
}


pub fn bind(s: &Value) -> Result<Vec<Member>, CaeError> {
    let mut out = Vec::new();
    let (mut start, mut force) = (0, 0);
    let rows = s["members"].as_array().cloned().unwrap_or_default();
    let layout = s["layout"].as_array().cloned().unwrap_or_default();
    for (row, lay) in rows.iter().zip(&layout) {
        let component = selected(row["component"].as_str().unwrap_or_default())?;
        let law = component.bind_law(&row["settings"])?;
        let stop = start + usize::try_from(lay["state_size"].as_u64().unwrap_or(0)).unwrap_or(0);
        let end = force + usize::try_from(lay["forcing_size"].as_u64().unwrap_or(0)).unwrap_or(0);
        out.push(Member {
            id: row["id"].as_str().unwrap_or_default().to_string(),
            component,
            law,
            state: start..stop,
            forcing: force..end,
        });
        start = stop;
        force = end;
    }
    Ok(out)
}


pub fn state_dependencies_for(s: &Value) -> Result<Vec<String>, CaeError> {
    let mut deps: Vec<String> = Vec::new();
    for row in s["members"].as_array().into_iter().flatten() {
        let component = selected(row["component"].as_str().unwrap_or_default())?;
        deps.extend(component.state_dependencies_for(&row["settings"])?);
    }
    deps.sort();
    deps.dedup();
    Ok(deps)
}

#[must_use]
pub fn state_metadata(members: &[Member]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in members {
        for mut row in m.law.state_metadata() {
            row["name"] = json!(format!("{}__{}", m.id, row["name"].as_str().unwrap_or_default()));
            out.push(row);
        }
    }
    out
}

#[must_use]
pub fn forcing(members: &[Member], s: &Value) -> (Vec<usize>, Vec<f64>) {
    let rows = s["members"].as_array().cloned().unwrap_or_default();
    let parts: Vec<(Vec<usize>, Vec<f64>)> =
        members.iter().zip(&rows).map(|(m, r)| m.law.forcing(&r["settings"])).collect();
    let Some((first, _)) = parts.first() else { return (Vec::new(), Vec::new()) };
    if first.len() != 3 || parts.iter().any(|(s, _)| s.len() != 3 || s[..2] != first[..2]) {
        return (Vec::new(), Vec::new());
    }
    let (nt, nc) = (first[0], first[1]);
    let channels: usize = parts.iter().map(|(s, _)| s[2]).sum();
    let mut out = Vec::with_capacity(nt * nc * channels);
    for point in 0..nt * nc {
        for (shape, data) in &parts {
            out.extend_from_slice(&data[point * shape[2]..(point + 1) * shape[2]]);
        }
    }
    (vec![nt, nc, channels], out)
}

#[allow(clippy::too_many_arguments)]
pub fn residual<S: Scalar>(
    members: &[Member],
    state: &[S],
    previous: &[S],
    temps: [S; 2],
    stress: &Mandel<S>,
    dt: S,
    forcing: &[S],
    out: &mut [S],
) {
    for m in members {
        m.law.residual(
            &state[m.state.clone()],
            &previous[m.state.clone()],
            temps,
            stress,
            dt,
            &forcing[m.forcing.clone()],
            &mut out[m.state.clone()],
        );
    }
}

pub fn properties<S: Scalar>(members: &[Member], endpoint: usize, state: &[S], base: [S; 3]) -> [S; 3] {
    let mut out = base;
    for m in members {
        let values = m.law.properties(endpoint, &state[m.state.clone()], base);
        for k in 0..3 {
            #[allow(clippy::float_cmp)]                                               
            let factor = if base[k].value() == 0.0 { S::one() } else { values[k] / base[k] };
            out[k] *= factor;
        }
    }
    out
}

pub fn energy<S: Scalar>(
    members: &[Member],
    state: &[S],
    temps: [S; 2],
    forcing: &[S],
    c: S,
    densities: [f64; 2],
) -> HistoryEnergy<S> {
    let mut out = HistoryEnergy::zero();
    for m in members {
        let e = m.law.energy(&state[m.state.clone()], temps, &forcing[m.forcing.clone()], c, densities);
        out.stored += e.stored;
        out.sensible_heat += e.sensible_heat;
        out.external += e.external;
    }
    out
}


pub fn check_state(members: &[Member], state: &[f64], width: usize) -> Result<(), CaeError> {
    let total: usize = members.iter().map(|m| m.state.len()).sum();
    if width != total || width == 0 || !state.len().is_multiple_of(width) {
        return contract("composite material-history state width disagrees with layout");
    }
    for m in members {
        let part: Vec<f64> =
            state.chunks(width).flat_map(|row| row[m.state.clone()].iter().copied()).collect();
        m.law.check_state(&part, m.state.len())?;
    }
    Ok(())
}
