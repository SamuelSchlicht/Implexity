// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value};

use implexity_core::py_repr::repr_str;

pub const SCHEMA: &str = "implexity-material-cards/1";
pub const COLLECTIONS: [&str; 1] = ["screening_metals"];

const SCREENING_METALS: &str = include_str!("../data/screening_metals.json");

const ELASTIC_THERMAL: [&str; 6] = [
    "youngs_modulus_Pa",
    "thermal_expansion_per_K",
    "poisson_ratio",
    "thermal_conductivity_W_per_m_K",
    "density_kg_per_m3",
    "reference_condition",
];
const FATIGUE: [&str; 4] = [
    "basquin_strength_coefficient_Pa",
    "basquin_exponent",
    "coffin_manson_ductility_coefficient",
    "coffin_manson_exponent",
];
const CREEP: [&str; 4] =
    ["larson_miller_constant", "master_curve_stress_Pa", "master_curve_parameter", "note"];
const CARD: [&str; 10] = [
    "id",
    "name",
    "aliases",
    "family",
    "tier",
    "elastic_thermal",
    "fatigue",
    "creep_rupture",
    "characteristic_values",
    "provenance",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MaterialCardError(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct MaterialCard {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub family: String,
    pub tier: String,
    pub collection: String,
    pub e_pa: f64,
    pub alpha_per_k: f64,
    pub nu: f64,
    pub k_w_per_m_k: f64,
    pub density_kg_per_m3: f64,
    pub reference_condition: String,
    pub sigma_f_prime_pa: f64,
    pub b: f64,
    pub eps_f_prime: f64,
    pub c: f64,
    pub larson_miller_c: f64,
    pub lmp_stress_curve: Vec<[f64; 2]>,
    pub creep_note: String,
    pub characteristic_values: BTreeMap<String, f64>,
    pub units: Map<String, Value>,
    pub sources: Vec<String>,
    pub references: BTreeMap<String, String>,
    pub provenance_statement: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaterialArrays {
    pub stations: BTreeMap<&'static str, Vec<f64>>,
    pub larson_miller_c: f64,
    pub lmp_stress_curve: Vec<[f64; 2]>,
}

impl MaterialCard {
    #[must_use]
    pub fn as_arrays(&self, n_stations: usize) -> MaterialArrays {
        let v = |x: f64| vec![x; n_stations];
        MaterialArrays {
            stations: BTreeMap::from([
                ("E_Pa", v(self.e_pa)),
                ("alpha_per_K", v(self.alpha_per_k)),
                ("nu", v(self.nu)),
                ("k_W_per_m_K", v(self.k_w_per_m_k)),
                ("sigma_f_prime_Pa", v(self.sigma_f_prime_pa)),
                ("b", v(self.b)),
                ("eps_f_prime", v(self.eps_f_prime)),
                ("c", v(self.c)),
            ]),
            larson_miller_c: self.larson_miller_c,
            lmp_stress_curve: self.lmp_stress_curve.clone(),
        }
    }


    pub fn characteristic(&self, name: &str) -> Result<f64, MaterialCardError> {
        self.characteristic_values.get(name).copied().ok_or_else(|| {
            let names: Vec<String> = self.characteristic_values.keys().map(|k| repr_str(k)).collect();
            MaterialCardError(format!(
                "material card {} has no {}; available: [{}]",
                repr_str(&self.id),
                repr_str(name),
                names.join(", ")
            ))
        })
    }
}

fn real(value: &Value, label: &str, positive: bool) -> Result<f64, MaterialCardError> {
    let v = match value {
        Value::Number(n) => n.as_f64().filter(|v| v.is_finite()),
        _ => None,
    }
    .ok_or_else(|| MaterialCardError(format!("{label} must be a finite real number")))?;
    if positive && v <= 0.0 {
        return Err(MaterialCardError(format!("{label} must be positive")));
    }
    Ok(v)
}

fn text(value: &Value, label: &str) -> Result<String, MaterialCardError> {
    match value.as_str() {
        Some(s) if !s.trim().is_empty() && s.chars().count() <= 4096 => Ok(s.to_string()),
        _ => Err(MaterialCardError(format!("{label} must be nonempty text"))),
    }
}

fn exact<'a>(
    row: &'a Value,
    keys: &[&str],
    label: &str,
) -> Result<&'a Map<String, Value>, MaterialCardError> {
    match row.as_object() {
        Some(m) if m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)) => Ok(m),
        _ => Err(MaterialCardError(format!("{label} requires exactly: {}", keys.join(", ")))),
    }
}

fn card(
    row: &Value,
    collection: &str,
    units: &Map<String, Value>,
    references: &Map<String, Value>,
) -> Result<MaterialCard, MaterialCardError> {
    let row = exact(row, &CARD, "material card")?;
    let ident = text(&row["id"], "card id")?;
    let aliases: Vec<String> = match row["aliases"].as_array() {
        Some(a) if a.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty())) => {
            a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()
        }
        _ => return Err(MaterialCardError(format!("{ident}: aliases must be a list of names"))),
    };
    let et = exact(&row["elastic_thermal"], &ELASTIC_THERMAL, &format!("{ident} elastic_thermal"))?;
    let fat = exact(&row["fatigue"], &FATIGUE, &format!("{ident} fatigue"))?;
    let creep = exact(&row["creep_rupture"], &CREEP, &format!("{ident} creep_rupture"))?;
    let list = |v: &Value, label: &str| -> Result<Vec<f64>, MaterialCardError> {
        v.as_array().map_or_else(Vec::new, Clone::clone).iter().map(|x| real(x, label, true)).collect()
    };
    let stress = list(&creep["master_curve_stress_Pa"], &format!("{ident} master-curve stress"))?;
    let lmp = list(&creep["master_curve_parameter"], &format!("{ident} master-curve parameter"))?;
    if stress.len() < 2 || stress.len() != lmp.len() || stress.windows(2).any(|w| w[1] <= w[0]) {
        return Err(MaterialCardError(format!(
            "{ident}: the master curve needs at least two rows with strictly increasing stress"
        )));
    }
    let curve: Vec<[f64; 2]> = stress.iter().zip(&lmp).map(|(s, p)| [s.log10(), *p]).collect();
    let characteristic = match row["characteristic_values"].as_object() {
        Some(m) if m.keys().all(|k| units.contains_key(k)) => m,
        _ => {
            return Err(MaterialCardError(format!(
                "{ident}: characteristic values must be named quantities with declared units"
            )));
        }
    };
    let prov = exact(&row["provenance"], &["sources", "statement"], &format!("{ident} provenance"))?;
    let sources: Vec<String> = match prov["sources"].as_array() {
        Some(s)
            if !s.is_empty() && s.iter().all(|x| x.as_str().is_some_and(|k| references.contains_key(k))) =>
        {
            s.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()
        }
        _ => return Err(MaterialCardError(format!("{ident}: provenance must cite bundled references"))),
    };
    let mut values = BTreeMap::new();
    for (k, v) in characteristic {
        values.insert(k.clone(), real(v, &format!("{ident} {k}"), false)?);
    }
    Ok(MaterialCard {
        name: text(&row["name"], &format!("{ident} name"))?,
        aliases,
        family: text(&row["family"], &format!("{ident} family"))?,
        tier: text(&row["tier"], &format!("{ident} tier"))?,
        collection: collection.into(),
        e_pa: real(&et["youngs_modulus_Pa"], &format!("{ident} E"), true)?,
        alpha_per_k: real(&et["thermal_expansion_per_K"], &format!("{ident} alpha"), false)?,
        nu: real(&et["poisson_ratio"], &format!("{ident} nu"), false)?,
        k_w_per_m_k: real(&et["thermal_conductivity_W_per_m_K"], &format!("{ident} k"), true)?,
        density_kg_per_m3: real(&et["density_kg_per_m3"], &format!("{ident} density"), true)?,
        reference_condition: text(&et["reference_condition"], &format!("{ident} reference condition"))?,
        sigma_f_prime_pa: real(&fat["basquin_strength_coefficient_Pa"], &format!("{ident} sigma_f'"), true)?,
        b: real(&fat["basquin_exponent"], &format!("{ident} b"), false)?,
        eps_f_prime: real(&fat["coffin_manson_ductility_coefficient"], &format!("{ident} eps_f'"), true)?,
        c: real(&fat["coffin_manson_exponent"], &format!("{ident} c"), false)?,
        larson_miller_c: real(&creep["larson_miller_constant"], &format!("{ident} Larson-Miller C"), true)?,
        lmp_stress_curve: curve,
        creep_note: text(&creep["note"], &format!("{ident} creep note"))?,
        characteristic_values: values,
        units: units.clone(),
        references: sources
            .iter()
            .map(|s| (s.clone(), references.get(s).and_then(Value::as_str).unwrap_or_default().to_string()))
            .collect(),
        sources,
        provenance_statement: text(&prov["statement"], &format!("{ident} provenance statement"))?,
        id: ident,
    })
}

fn load_collection(name: &str, source: &str) -> Result<Vec<MaterialCard>, MaterialCardError> {
    let violates = || MaterialCardError(format!("material collection {} violates {SCHEMA}", repr_str(name)));
    let doc = implexity_core::json::parse_strict(source).map_err(|_| violates())?;
    let keys = ["schema", "collection", "description", "units", "references", "cards"];
    let d = doc.as_object().filter(|d| d.len() == keys.len() && keys.iter().all(|k| d.contains_key(*k)));
    let Some(d) =
        d.filter(|d| d["schema"].as_str() == Some(SCHEMA) && d["collection"].as_str() == Some(name))
    else {
        return Err(violates());
    };
    let units = d["units"].as_object().cloned().unwrap_or_default();
    let references = d["references"].as_object().cloned().unwrap_or_default();
    d["cards"]
        .as_array()
        .map_or_else(Vec::new, Clone::clone)
        .iter()
        .map(|row| card(row, name, &units, &references))
        .collect()
}

type Index = BTreeMap<String, Arc<MaterialCard>>;

static INDEX: LazyLock<Result<Index, MaterialCardError>> = LazyLock::new(|| {
    let mut by_name: Index = BTreeMap::new();
    for collection in COLLECTIONS {
        let source = match collection {
            "screening_metals" => SCREENING_METALS,
            _ => continue,
        };
        for card in load_collection(collection, source)? {
            let card = Arc::new(card);
            for key in std::iter::once(&card.id).chain(&card.aliases) {
                if by_name.contains_key(key) {
                    return Err(MaterialCardError(format!(
                        "duplicate material card id or alias {}",
                        repr_str(key)
                    )));
                }
                by_name.insert(key.clone(), Arc::clone(&card));
            }
        }
    }
    Ok(by_name)
});

fn index() -> Result<&'static Index, MaterialCardError> {
    INDEX.as_ref().map_err(Clone::clone)
}


pub fn material_card_ids() -> Result<Vec<String>, MaterialCardError> {
    let ids: std::collections::BTreeSet<String> = index()?.values().map(|c| c.id.clone()).collect();
    Ok(ids.into_iter().collect())
}


pub fn material_card_names() -> Result<Vec<String>, MaterialCardError> {
    Ok(index()?.keys().cloned().collect())
}


pub fn material_card(name: &str) -> Result<Arc<MaterialCard>, MaterialCardError> {
    let idx = index()?;
    idx.get(name).cloned().ok_or_else(|| {
        let known: Vec<String> = idx.keys().map(|k| repr_str(k)).collect();
        MaterialCardError(format!("unknown material card {}; known: [{}]", repr_str(name), known.join(", ")))
    })
}


pub fn material_cards(collection: Option<&str>) -> Result<Vec<Arc<MaterialCard>>, MaterialCardError> {
    if let Some(c) = collection
        && !COLLECTIONS.contains(&c)
    {
        return Err(MaterialCardError(format!(
            "unknown material collection {}; known: ['screening_metals']",
            repr_str(c)
        )));
    }
    let mut cards: BTreeMap<String, Arc<MaterialCard>> = BTreeMap::new();
    for card in index()?.values() {
        cards.insert(card.id.clone(), Arc::clone(card));
    }
    Ok(cards.into_values().filter(|c| collection.is_none_or(|x| c.collection == x)).collect())
}

