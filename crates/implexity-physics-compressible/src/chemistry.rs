// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use serde_json::{Map, Value, json};
use crate::errors::{PResult, ModelError};
use crate::pyval::{num, py_float, strip};
pub const ATOMIC_MASS_KG_PER_KMOL: [(&str, f64); 12] = [
    ("H", 1.00794),
    ("C", 12.0107),
    ("N", 14.0067),
    ("O", 15.9994),
    ("F", 18.998_403_2),
    ("Al", 26.981_538_6),
    ("Si", 28.0855),
    ("P", 30.973_762),
    ("S", 32.065),
    ("Cl", 35.453),
    ("Ar", 39.948),
    ("B", 10.811),
];

fn atomic_mass(element: &str) -> Option<f64> {
    ATOMIC_MASS_KG_PER_KMOL.iter().find(|(e, _)| *e == element).map(|(_, m)| *m)
}


pub fn finite_number(value: Option<&Value>, path: &str) -> PResult<f64> {
    let out = value
        .and_then(py_float)
        .ok_or_else(|| ModelError::validation(format!("{path} must be numeric"), path))?;
    if !out.is_finite() {
        return Err(ModelError::validation(format!("{path} must be finite"), path));
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct BalanceResult {
    pub element_residuals: Vec<(String, f64)>,
    pub mass_residual_kg_per_kmol: f64,
    pub scale: f64,
}

impl BalanceResult {
    #[must_use]
    pub fn balanced(&self) -> bool {
        let tol = 1.0e-12_f64.max(1.0e-9 * self.scale);
        self.element_residuals.iter().all(|(_, v)| v.abs() <= tol)
    }

    #[must_use]
    pub fn as_dict(&self) -> Map<String, Value> {
        let mut residuals = Map::new();
        for (k, v) in &self.element_residuals {
            residuals.insert(k.clone(), num(*v));
        }
        let mut out = Map::new();
        out.insert("balanced".into(), json!(self.balanced()));
        out.insert("elementResiduals".into(), Value::Object(residuals));
        out.insert("massResidualKgPerKmol".into(), num(self.mass_residual_kg_per_kmol));
        out.insert("scale".into(), num(self.scale));
        out
    }
}


pub fn normalize_composition(composition: Option<&Value>, path: &str) -> PResult<Map<String, Value>> {
    normalize_composition_with(composition, path, 1.0e-8)
}


pub fn normalize_composition_with(
    composition: Option<&Value>,
    path: &str,
    tolerance: f64,
) -> PResult<Map<String, Value>> {
    let map = composition
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| ModelError::validation("A non-empty species composition is required", path))?;
    let mut out: Map<String, Value> = Map::new();
    let mut values: Vec<(String, f64)> = Vec::new();
    for (species, raw) in map {
        let name = strip(species).to_string();
        if name.is_empty() {
            return Err(ModelError::validation("Species names may not be empty", path));
        }
        let value = finite_number(Some(raw), &format!("{path}.{name}"))?;
        if value < 0.0 {
            return Err(ModelError::validation(
                "Species fractions may not be negative",
                format!("{path}.{name}"),
            ));
        }
        if let Some(slot) = values.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = value;
        } else {
            values.push((name.clone(), value));
        }
        out.insert(name, num(value));
    }
    let total: f64 = values.iter().map(|(_, v)| v).sum();
    if total <= 0.0 {
        return Err(ModelError::validation("Species fractions must have a positive sum", path));
    }
    if (total - 1.0).abs() > tolerance {
        return Err(ModelError::validation(
            "Species fractions must sum to one in the declared basis",
            path,
        )
        .detail("sum", num(total))
        .detail("tolerance", num(tolerance)));
    }
    Ok(out)
}


pub fn species_molar_mass(elements: Option<&Value>, path: &str) -> PResult<f64> {
    let map = elements
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| ModelError::validation("Species elemental composition is required", path))?;
    let mut mass = 0.0;
    for (element, raw_count) in map {
        let Some(atomic) = atomic_mass(element) else {
            return Err(ModelError::validation(
                format!(
                    "Unknown element {}; provide it through the high-fidelity mechanism backend",
                    implexity_core::py_repr::repr_str(element)
                ),
                format!("{path}.{element}"),
            ));
        };
        let count = finite_number(Some(raw_count), &format!("{path}.{element}"))?;
        if count < 0.0 {
            return Err(ModelError::validation(
                "Element counts may not be negative",
                format!("{path}.{element}"),
            ));
        }
        mass += atomic * count;
    }
    if mass <= 0.0 {
        return Err(ModelError::validation("Species molar mass must be positive", path));
    }
    Ok(mass)
}


pub fn balance_global_reaction(
    species: &Map<String, Value>,
    stoichiometry: Option<&Value>,
) -> PResult<BalanceResult> {
    let stoich = stoichiometry.and_then(Value::as_object).filter(|m| !m.is_empty()).ok_or_else(|| {
        ModelError::validation(
            "A global reaction requires stoichiometric coefficients",
            "chemistry.stoichiometry",
        )
    })?;
    let mut residuals: Vec<(String, f64)> = Vec::new();
    let mut mass_residual = 0.0;
    let mut scale = 0.0;
    let (mut has_reactant, mut has_product) = (false, false);
    for (name, raw_nu) in stoich {
        let nu = finite_number(Some(raw_nu), &format!("chemistry.stoichiometry.{name}"))?;
        if nu.abs() <= 1.0e-15 {
            continue;
        }
        has_reactant |= nu < 0.0;
        has_product |= nu > 0.0;
        let Some(definition) = species.get(name) else {
            return Err(ModelError::validation(
                format!(
                    "Reaction species {} has no elemental definition",
                    implexity_core::py_repr::repr_str(name)
                ),
                format!("chemistry.species.{name}"),
            ));
        };
        let elements = definition.get("elements");
        let molar_mass = species_molar_mass(elements, &format!("chemistry.species.{name}.elements"))?;
        mass_residual += nu * molar_mass;
        scale += nu.abs() * molar_mass;
        if let Some(map) = elements.and_then(Value::as_object) {
            for (element, raw_count) in map {
                let count =
                    finite_number(Some(raw_count), &format!("chemistry.species.{name}.elements.{element}"))?;
                if let Some(slot) = residuals.iter_mut().find(|(e, _)| e == element) {
                    slot.1 += nu * count;
                } else {
                    residuals.push((element.clone(), nu * count));
                }
            }
        }
    }
    if !has_reactant || !has_product {
        return Err(ModelError::validation(
            "Reaction coefficients must contain negative reactants and positive products",
            "chemistry.stoichiometry",
        ));
    }
    let result = BalanceResult {
        element_residuals: residuals,
        mass_residual_kg_per_kmol: mass_residual,
        scale: scale.max(1.0),
    };
    if !result.balanced() {
        return Err(ModelError::chemistry_balance(
            "The declared global reaction is not elementally balanced",
            "chemistry.stoichiometry",
        )
        .with_details(result.as_dict()));
    }
    Ok(result)
}


pub fn validate_arrhenius(arrhenius: Option<&Value>) -> PResult<Map<String, Value>> {
    let empty = Map::new();
    let arr = arrhenius.and_then(Value::as_object).unwrap_or(&empty);
    let required = ["A", "temperatureExponent", "activationEnergyJPerMol"];
    let missing: Vec<&str> = required.iter().copied().filter(|n| !arr.contains_key(*n)).collect();
    if !missing.is_empty() {
        return Err(ModelError::validation(
            "Arrhenius declaration is incomplete",
            "chemistry.arrhenius",
        )
        .detail("missing", json!(missing)));
    }
    let mut out = Map::new();
    let mut vals = [0.0; 3];
    for (i, name) in required.iter().enumerate() {
        vals[i] = finite_number(arr.get(*name), &format!("chemistry.arrhenius.{name}"))?;
        out.insert((*name).to_string(), num(vals[i]));
    }
    if vals[0] <= 0.0 {
        return Err(ModelError::validation(
            "Arrhenius pre-exponential factor must be positive",
            "chemistry.arrhenius.A",
        ));
    }
    if vals[2] < 0.0 {
        return Err(ModelError::validation(
            "Activation energy may not be negative",
            "chemistry.arrhenius.activationEnergyJPerMol",
        ));
    }
    Ok(out)
}

pub const SCREENING_ARRHENIUS_FORMULA: &str = "k = A_eff * exp(-Ea / (R * T_ref))";

pub const SCREENING_DEFAULT_REFERENCE_TEMPERATURE_K: f64 = 2500.0;

pub const RATE_SCALE_AGREEMENT_RTOL: f64 = 1.0e-9;
