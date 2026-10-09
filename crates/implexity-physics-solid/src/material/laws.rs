// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_core::pyobj::list_repr;

use super::caloric::{affine, numerical_table, phase, table};
use super::record::{
    MATERIAL_KEYS, PhaseTransition, Props, SolidMaterial, TEMPERATURE_KEYS, TableCurves,
    finite_number, idx, temperature_slot, validate_material,
};
use crate::mandel::{self, Mandel};
use crate::pchip::PropertyCurve;

fn contract<T>(message: impl Into<String>) -> Result<T, CaeError> {
    Err(CaeError::contract(message))
}

pub const TABLE_UNITS: [(&str, &str); 5] = [
    ("E", "Pa"),
    ("yield_stress", "Pa"),
    ("alpha", "1/K"),
    ("k", "W/(m K)"),
    ("cp", "J/(kg K)"),
];
pub const INACTIVE_SOLID_NUMERICAL_MATERIAL_SCHEMA: &str =
    "implexity-inactive-solid-endmember-numerical-material/1";
pub const INACTIVE_SOLID_NUMERICAL_MATERIAL_METHOD: &str = "positive_c1_endpoint_tangent_tanh";
pub const INACTIVE_SOLID_NUMERICAL_MATERIAL_SCOPE: &str =
    "exact_zero_physical_solid_or_endmember_support_only";
pub const KIN_KEYS: [&str; 5] = [
    "C_Pa",
    "gamma",
    "interpolation",
    "provenance",
    "temperature_K",
];

pub fn normalise_inactive_solid_numerical_material(value: &Value) -> Result<Value, CaeError> {
    let keys = ["method", "provenance", "schema", "scope"];
    let Some(v) = value
        .as_object()
        .filter(|v| v.len() == 4 && keys.iter().all(|k| v.contains_key(*k)))
    else {
        return contract(format!(
            "inactive solid/endmember numerical material requires exactly {}",
            list_repr(&keys)
        ));
    };
    if v["schema"].as_str() != Some(INACTIVE_SOLID_NUMERICAL_MATERIAL_SCHEMA) {
        return contract(format!(
            "inactive solid/endmember numerical material schema must be {INACTIVE_SOLID_NUMERICAL_MATERIAL_SCHEMA}"
        ));
    }
    if v["method"].as_str() != Some(INACTIVE_SOLID_NUMERICAL_MATERIAL_METHOD) {
        return contract(format!(
            "inactive solid/endmember numerical material method must be {INACTIVE_SOLID_NUMERICAL_MATERIAL_METHOD}"
        ));
    }
    if v["scope"].as_str() != Some(INACTIVE_SOLID_NUMERICAL_MATERIAL_SCOPE) {
        return contract(format!(
            "inactive solid/endmember numerical material scope must be {INACTIVE_SOLID_NUMERICAL_MATERIAL_SCOPE}"
        ));
    }
    if !v["provenance"]
        .as_str()
        .is_some_and(|p| !p.trim().is_empty())
    {
        return contract("inactive solid/endmember numerical material provenance required");
    }
    Ok(value.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialLaw {
    TemperatureLinear,
    PhaseTransition,
    ConstantStrainThermoelastic,
    TemperatureTable,
    ChabocheTable,
}

fn strings(items: &[&str]) -> Value {
    json!(items)
}

fn support(history: bool, limitations: &[&str]) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("status".into(), json!("field_component"));
    m.insert("history".into(), json!(history));
    m.insert("data".into(), json!("user_required"));
    m.insert("limitations".into(), strings(limitations));
    m
}

pub struct ThermoelasticCoefficients<S> {
    pub g: S,
    pub k: S,
    pub beta: S,
    pub capacity: S,
}

impl MaterialLaw {
    pub const ALL: [Self; 5] = [
        Self::TemperatureLinear,
        Self::PhaseTransition,
        Self::ConstantStrainThermoelastic,
        Self::TemperatureTable,
        Self::ChabocheTable,
    ];

    #[must_use]
    pub fn component_id(self) -> &'static str {
        match self {
            Self::TemperatureLinear => "temperature_linear_solid",
            Self::PhaseTransition => "phase_transition_caloric_solid",
            Self::ConstantStrainThermoelastic => "constant_strain_thermoelastic_solid",
            Self::TemperatureTable => "temperature_tabulated_solid",
            Self::ChabocheTable => "temperature_tabulated_chaboche_solid",
        }
    }

    #[must_use]
    pub fn implementation(self) -> &'static str {
        match self {
            Self::TemperatureLinear => {
                "implexity.physics_library.inelastic_components.TemperatureLinearSolid"
            }
            Self::PhaseTransition => {
                "implexity.physics_library.inelastic_components.PhaseTransitionCaloricSolid"
            }
            Self::ConstantStrainThermoelastic => {
                "implexity.physics_library.analytic_thermoelastic.ConstantStrainThermoelasticSolid"
            }
            Self::TemperatureTable => {
                "implexity.physics_library.tabulated_materials.TemperatureTableSolid"
            }
            Self::ChabocheTable => "implexity.physics_library.chaboche.ChabocheTableSolid",
        }
    }

    #[must_use]
    pub fn limitations(self) -> &'static [&'static str] {
        match self {
            Self::TemperatureLinear => &["Linear temperature laws only, no extrapolation."],
            Self::PhaseTransition => &[
                "Smooth equilibrium latent-enthalpy transition with fixed density and inherited solid mechanics.",
                "No liquid flow, phase-dependent stiffness, transformation strain, hysteresis or kinetic phase evolution.",
            ],
            Self::ConstantStrainThermoelastic => &[
                "Constant coefficients; explicitly authored volumetric constant-strain heat capacity replaces cp.",
                "Backward-Euler entropy storage; separate numerical energy defect, not dissipative heat.",
                "No simultaneous plasticity, creep, Maxwell, material history or numerical continuation.",
            ],
            Self::TemperatureTable => &[
                "C1 PCHIP temperature interpolation without extrapolation; input tables are NOT automatically validated material data.",
                "Exact caloric and thermal-strain integrals; density, Poisson ratio, hardening and Norton constants remain constant.",
                "No irradiation, rupture, calibrated cyclic fatigue or phase transformation is implied.",
            ],
            Self::ChabocheTable => &[
                "All elastic/caloric/yield tables and nonlinear hardening data must be explicitly authored.",
                "1..8 PCHIP kinematic branches; no extrapolation, fatigue, damage or implicit calibration.",
                "Common mixed-property continuum is an optimisation relaxation, not a bonded dissimilar-material joint model.",
            ],
        }
    }

    #[must_use]
    pub fn runtime_support(self) -> Map<String, Value> {
        support(false, self.limitations())
    }

    #[must_use]
    pub fn authoring_contract(self) -> Option<Map<String, Value>> {
        (self == Self::ChabocheTable).then(|| {
            json!({"schema": "implexity-component-authoring/1", "selection": "solid.components.material",
                "base_contract": "temperature_tabulated_solid",
                "additional_material_keys": ["kinematic_hardening"],
                "hardening_required_keys": KIN_KEYS,
                "units": {"temperature_K": "K", "C_Pa": "Pa", "gamma": "1"},
                "hardening_array_shape": ["temperature", "ordered_backstress_branch"],
                "branch_count": [1, 8], "legacy_H_iso_H_kin_must_be_zero": true,
                "interpolation": "C1 PCHIP; no extrapolation; coefficients are not an automatic fit"})
            .as_object()
            .cloned()
            .unwrap_or_default()
        })
    }

    #[must_use]
    pub fn reversible_thermoelastic(self) -> bool {
        self == Self::ConstantStrainThermoelastic
    }

    #[must_use]
    pub fn supports_numerical_material(self) -> bool {
        matches!(self, Self::TemperatureTable | Self::ChabocheTable)
    }

    pub fn validate(self, raw: &Value) -> Result<SolidMaterial, CaeError> {
        match self {
            Self::TemperatureLinear => Ok(SolidMaterial::from_validated(validate_material(raw)?)),
            Self::PhaseTransition => validate_phase(raw),
            Self::ConstantStrainThermoelastic => validate_constant_strain(raw),
            Self::TemperatureTable => validate_table(raw),
            Self::ChabocheTable => validate_chaboche(raw),
        }
    }

    pub fn validate_bindings(self, plasticity: Option<&str>) -> Result<(), CaeError> {
        if self == Self::ChabocheTable && plasticity != Some("j2_chaboche") {
            return contract(
                "Chaboche material coefficients require explicit j2_chaboche; a linear or disabled plastic law would ignore the supplied branches",
            );
        }
        Ok(())
    }

    pub fn properties<S: Scalar>(self, m: &SolidMaterial, t: S) -> Props<S> {
        let mut props = self.base_properties(m, t);
        props.creep_curve = m.creep_curve.map(|c| c.map(S::from_f64));
        props.creep_validity = m.creep_validity;
        props
    }

    fn base_properties<S: Scalar>(self, m: &SolidMaterial, t: S) -> Props<S> {
        match self {
            Self::TemperatureLinear | Self::PhaseTransition | Self::ConstantStrainThermoelastic => {
                let mut values: [S; 15] = std::array::from_fn(|i| {
                    let base = m.values[i];
                    match temperature_slot(i) {
                        Some(slot) => (t - m.t_ref) * m.slopes[slot] + base,
                        None => S::from_f64(base),
                    }
                });
                if let (Self::PhaseTransition, Some(p)) = (self, m.phase) {
                    let x = (t - p.temperature) / (2.0 * p.width);
                    values[idx::CP] +=
                        (-(x.tanh().powi(2)) + 1.0) * p.latent_heat / (4.0 * p.width);
                }
                Props::new(values, t)
            }
            Self::TemperatureTable | Self::ChabocheTable => {
                let mut props = Props::new(table_values(m, t, PropertyCurve::value), t);
                if self == Self::ChabocheTable
                    && let Some(kin) = &m.kinematic
                {
                    props.kin_c = kin.iter().map(|(c, _)| c.value(t)).collect();
                    props.kin_gamma = kin.iter().map(|(_, g)| g.value(t)).collect();
                }
                props
            }
        }
    }

    pub fn thermal_strain<S: Scalar>(self, m: &SolidMaterial, t: S) -> S {
        match self {
            Self::TemperatureTable | Self::ChabocheTable => {
                let a = &curves(m)[2];
                a.primitive(t) - a.primitive(m.t_ref)
            }
            _ => {
                let dt = t - m.t_ref;
                dt * m.values[idx::ALPHA] + dt * dt * (0.5 * m.slopes[2])
            }
        }
    }

    pub fn sensible_enthalpy<S: Scalar>(self, m: &SolidMaterial, t: S) -> Result<S, CaeError> {
        match self {
            Self::ConstantStrainThermoelastic => contract(
                "Helmholtz thermoelastic storage must use the entropy contract, not sensible enthalpy",
            ),
            Self::TemperatureTable | Self::ChabocheTable => {
                let cp = &curves(m)[4];
                Ok(cp.primitive(t) - cp.primitive(m.t_ref))
            }
            Self::TemperatureLinear | Self::PhaseTransition => {
                let dt = t - m.t_ref;
                let mut h = dt * m.values[idx::CP] + dt * dt * (0.5 * m.slopes[4]);
                if let (Self::PhaseTransition, Some(p)) = (self, m.phase) {
                    h += (fraction(&p, t) - fraction(&p, S::from_f64(m.t_ref))) * p.latent_heat;
                }
                Ok(h)
            }
        }
    }

    pub fn enthalpy_increment<S: Scalar>(
        self,
        m: &SolidMaterial,
        t: S,
        tp: S,
    ) -> Result<S, CaeError> {
        match self {
            Self::ConstantStrainThermoelastic => contract(
                "Helmholtz thermoelastic storage must replace, not add to, enthalpy capacity",
            ),
            Self::TemperatureTable | Self::ChabocheTable => {
                let cp = &curves(m)[4];
                Ok(cp.primitive(t) - cp.primitive(tp))
            }
            Self::TemperatureLinear | Self::PhaseTransition => {
                let mut h =
                    (((t + tp) * 0.5 - m.t_ref) * m.slopes[4] + m.values[idx::CP]) * (t - tp);
                if let (Self::PhaseTransition, Some(p)) = (self, m.phase) {
                    h += (fraction(&p, t) - fraction(&p, tp)) * p.latent_heat;
                }
                Ok(h)
            }
        }
    }

    pub fn enthalpy_increment_from_delta<S: Scalar>(
        self,
        m: &SolidMaterial,
        previous: S,
        delta: S,
    ) -> Result<S, CaeError> {
        match self {
            Self::ConstantStrainThermoelastic => contract(
                "Helmholtz thermoelastic storage must replace, not add to, enthalpy capacity",
            ),
            Self::TemperatureLinear => Ok(affine(m, previous, delta)),
            Self::PhaseTransition => Ok(m.phase.map_or_else(
                || affine(m, previous, delta),
                |p| phase(m, &p, previous, delta),
            )),
            Self::TemperatureTable | Self::ChabocheTable => {
                Ok(table(&curves(m)[4], previous, delta))
            }
        }
    }

    pub fn validate_numerical_material(
        self,
        m: &SolidMaterial,
        policy: &Value,
    ) -> Result<Value, CaeError> {
        if !self.supports_numerical_material() {
            return contract(
                "selected solid material does not support inactive-phase/endmember numerical continuation",
            );
        }
        normalise_inactive_solid_numerical_material(policy)?;
        let c = curves(m);
        for (slot, key) in TEMPERATURE_KEYS.iter().enumerate() {
            let curve = &c[slot];
            let mut probe = vec![m.t_min];
            probe.extend(
                curve
                    .knots
                    .iter()
                    .copied()
                    .filter(|k| *k > m.t_min && *k < m.t_max),
            );
            probe.push(m.t_max);
            let authored: Vec<f64> = probe.iter().map(|t| curve.value(*t)).collect();
            let lo = curve.value(m.t_min);
            let hi = curve.value(m.t_max);
            let ds = [curve.derivative(m.t_min), curve.derivative(m.t_max)];
            let ok = authored.iter().all(Scalar::is_finite)
                && authored.iter().copied().fold(f64::INFINITY, f64::min) > 0.0
                && lo.min(hi) > 0.0
                && [lo, hi, ds[0], ds[1]].iter().all(Scalar::is_finite);
            if !ok {
                return contract(format!(
                    "inactive solid/endmember continuation requires positive finite {key} throughout its authored validity interval and finite endpoint tangents"
                ));
            }
        }
        Ok(policy.clone())
    }

    pub fn numerical_properties<S: Scalar>(self, m: &SolidMaterial, t: S) -> Props<S> {
        {
            let mut props = Props::new(
                table_values(m, t, |curve, t| continued_value(curve, t, m)),
                t,
            );
            props.creep_curve = m.creep_curve.map(|c| c.map(S::from_f64));
            props.creep_validity = m.creep_validity;
            props
        }
    }

    pub fn numerical_thermal_strain<S: Scalar>(self, m: &SolidMaterial, t: S) -> S {
        let a = &curves(m)[2];
        continued_primitive(a, t, m) - a.primitive(m.t_ref)
    }

    pub fn numerical_sensible_enthalpy<S: Scalar>(self, m: &SolidMaterial, t: S) -> S {
        let cp = &curves(m)[4];
        continued_primitive(cp, t, m) - cp.primitive(m.t_ref)
    }

    pub fn numerical_enthalpy_increment<S: Scalar>(self, m: &SolidMaterial, t: S, tp: S) -> S {
        let cp = &curves(m)[4];
        continued_primitive(cp, t, m) - continued_primitive(cp, tp, m)
    }

    pub fn numerical_enthalpy_increment_from_delta<S: Scalar>(
        self,
        m: &SolidMaterial,
        previous: S,
        delta: S,
    ) -> S {
        numerical_table(&curves(m)[4], m.t_min, m.t_max, previous, delta)
    }

    #[must_use]
    pub fn extra_keys(self) -> &'static [&'static str] {
        match self {
            Self::PhaseTransition => &[
                "latent_heat_J_kg",
                "transition_temperature_K",
                "transition_width_K",
            ],
            Self::ConstantStrainThermoelastic => &["constant_strain_heat_capacity_J_m3_K"],
            Self::ChabocheTable => &["kinematic_hardening"],
            _ => &[],
        }
    }
}

fn fraction<S: Scalar>(p: &PhaseTransition, t: S) -> S {
    (((t - p.temperature) / (2.0 * p.width)).tanh() + 1.0) * 0.5
}

fn curves(m: &SolidMaterial) -> &TableCurves {
    m.tables.as_deref().unwrap_or_else(|| empty_curves())
}

fn empty_curves() -> &'static TableCurves {
    static EMPTY: std::sync::LazyLock<TableCurves> = std::sync::LazyLock::new(|| {
        std::array::from_fn(|_| PropertyCurve {
            knots: vec![f64::NAN, f64::NAN],
            coefficients: std::array::from_fn(|_| vec![f64::NAN]),
            integral_coefficients: std::array::from_fn(|_| vec![f64::NAN]),
        })
    });
    &EMPTY
}

fn table_values<S: Scalar>(m: &SolidMaterial, t: S, f: impl Fn(&PropertyCurve, S) -> S) -> [S; 15] {
    let c = curves(m);
    std::array::from_fn(|i| match temperature_slot(i) {
        Some(slot) => f(&c[slot], t),
        None => S::from_f64(m.values[i]),
    })
}

fn continued_endpoint<S: Scalar>(
    curve: &PropertyCurve,
    tb: f64,
    outward_distance: S,
    outward_derivative: f64,
) -> S {
    let qb = curve.value(tb);
    let d = outward_derivative;
    #[allow(clippy::float_cmp)]
    if d == 0.0 {
        return S::from_f64(qb);
    }
    let floor = f64::EPSILON * qb;
    let l = (qb - floor) / d.abs();
    (outward_distance / l).tanh() * (d * l) + qb
}

fn continued_value<S: Scalar>(curve: &PropertyCurve, t: S, m: &SolidMaterial) -> S {
    let v = t.value();
    if v < m.t_min {
        continued_endpoint(curve, m.t_min, -t + m.t_min, -curve.derivative(m.t_min))
    } else if v > m.t_max {
        continued_endpoint(curve, m.t_max, t - m.t_max, curve.derivative(m.t_max))
    } else {
        curve.value(t)
    }
}

fn outward_primitive<S: Scalar>(curve: &PropertyCurve, tb: f64, s: S, d: f64) -> S {
    let s = s.max_f64(0.0);
    let qb = curve.value(tb);
    #[allow(clippy::float_cmp)]
    if d == 0.0 {
        return s * qb;
    }
    let floor = f64::EPSILON * qb;
    let l = (qb - floor) / d.abs();
    let asymptote = qb + d * l;
    let tail = (s * -2.0 / l).exp().ln_1p() - std::f64::consts::LN_2;
    s * asymptote + tail * (d * l * l)
}

fn continued_primitive<S: Scalar>(curve: &PropertyCurve, t: S, m: &SolidMaterial) -> S {
    let v = t.value();
    if v < m.t_min {
        -outward_primitive(curve, m.t_min, -t + m.t_min, -curve.derivative(m.t_min))
            + curve.primitive(m.t_min)
    } else if v > m.t_max {
        outward_primitive(curve, m.t_max, t - m.t_max, curve.derivative(m.t_max))
            + curve.primitive(m.t_max)
    } else {
        curve.primitive(t)
    }
}

fn validate_phase(raw: &Value) -> Result<SolidMaterial, CaeError> {
    let Some(r) = raw.as_object() else {
        return contract("material data must be an object");
    };
    let keys = [
        "latent_heat_J_kg",
        "transition_temperature_K",
        "transition_width_K",
    ];
    if !keys.iter().all(|k| r.contains_key(*k)) {
        return contract("explicit phase-transition temperature, width and latent heat required");
    }
    let values: Vec<Option<f64>> = keys.iter().map(|k| finite_number(&r[*k])).collect();
    if values.iter().any(Option::is_none) {
        return contract("phase-transition coefficients must be finite real scalars");
    }
    let base_raw: Map<String, Value> = r
        .iter()
        .filter(|(k, _)| !keys.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut base = validate_material(&Value::Object(base_raw))?;
    let v = |i: usize| values[i].unwrap_or(f64::NAN);
    let (latent, t, width) = (v(0), v(1), v(2));
    let t_min = finite_number(&base["T_min"]).unwrap_or(f64::NAN);
    let t_max = finite_number(&base["T_max"]).unwrap_or(f64::NAN);
    if !(t_min < t && t < t_max) || width <= 0.0 || latent < 0.0 {
        return contract("invalid phase-transition interval, width or latent heat");
    }
    for k in keys {
        base.insert(k.into(), r[k].clone());
    }
    let mut m = SolidMaterial::from_validated(base);
    m.phase = Some(PhaseTransition {
        temperature: t,
        width,
        latent_heat: latent,
    });
    Ok(m)
}

fn validate_constant_strain(raw: &Value) -> Result<SolidMaterial, CaeError> {
    let Some(r) = raw.as_object().filter(|r| !r.contains_key("cp")) else {
        return contract("Author constant_strain_heat_capacity_J_m3_K, not cp, for this material");
    };
    let mut data = r.clone();
    let capacity = data
        .shift_remove("constant_strain_heat_capacity_J_m3_K")
        .as_ref()
        .and_then(finite_number);
    let Some(capacity) = capacity.filter(|c| *c > 0.0) else {
        return contract(
            "Positive finite volumetric constant-strain heat capacity [J/(m3 K)] required",
        );
    };
    let Some(density) = data
        .get("density")
        .and_then(finite_number)
        .filter(|d| *d > 0.0)
    else {
        return contract("Positive density required");
    };
    data.insert("cp".into(), json!(capacity / density));
    let validated = validate_material(&Value::Object(data))?;
    let slopes = validated["temperature_slopes"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    #[allow(clippy::float_cmp)]
    if slopes
        .values()
        .any(|v| finite_number(v).is_none_or(|x| x != 0.0))
    {
        return contract(
            "This Helmholtz model requires zero temperature slopes for all coefficients",
        );
    }
    let mut m = SolidMaterial::from_validated(r.clone());
    m.values[idx::CP] = capacity / density;
    m.constant_strain_capacity = Some(capacity);
    Ok(m)
}

fn validate_table(raw: &Value) -> Result<SolidMaterial, CaeError> {
    let Some(r) = raw.as_object() else {
        return contract("tabulated solid must be an object");
    };
    let mut required: Vec<&str> = MATERIAL_KEYS
        .iter()
        .copied()
        .filter(|k| !TEMPERATURE_KEYS.contains(k))
        .collect();
    required.extend([
        "name",
        "provenance",
        "T_ref",
        "T_min",
        "T_max",
        "temperature_table",
        "temperature_table_units",
    ]);
    let missing: Vec<&str> = {
        let mut v: Vec<&str> = required
            .iter()
            .copied()
            .filter(|k| !r.contains_key(*k))
            .collect();
        v.sort_unstable();
        v
    };
    let unsupported: Vec<&str> = {
        let mut v: Vec<&str> = r
            .keys()
            .map(String::as_str)
            .filter(|k| !required.contains(k))
            .collect();
        v.sort_unstable();
        v
    };
    if !missing.is_empty() || !unsupported.is_empty() {
        return contract(format!(
            "tabulated solid missing fields {}; unsupported fields {}",
            list_repr(&missing),
            list_repr(&unsupported)
        ));
    }
    let units_ok = r["temperature_table_units"].as_object().is_some_and(|u| {
        u.len() == TABLE_UNITS.len()
            && TABLE_UNITS
                .iter()
                .all(|(k, v)| u.get(*k).and_then(Value::as_str) == Some(v))
    });
    if !units_ok {
        return contract("tabulated solid units must match the declared SI property units");
    }
    let text_ok = |k: &str| r[k].as_str().is_some_and(|s| !s.trim().is_empty());
    if !text_ok("name") || !text_ok("provenance") {
        return contract("nonempty tabulated material name/provenance required");
    }
    let table = r["temperature_table"].as_object();
    let table_ok = table.is_some_and(|t| {
        t.len() == TEMPERATURE_KEYS.len() + 1
            && t.contains_key("temperature_K")
            && TEMPERATURE_KEYS.iter().all(|k| t.contains_key(*k))
    });
    let Some(table) = table.filter(|_| table_ok) else {
        return contract("all five tabulated temperature properties must be supplied explicitly");
    };
    for key in ["T_ref", "T_min", "T_max"] {
        if finite_number(&r[key]).is_none() {
            return contract("finite temperature reference and validity bounds required");
        }
    }
    let built: Vec<PropertyCurve> = TEMPERATURE_KEYS
        .iter()
        .map(|k| PropertyCurve::from_json(&table["temperature_K"], &table[*k]))
        .collect::<Result<_, _>>()?;
    let curves: TableCurves = std::array::from_fn(|i| built[i].clone());
    let knots = &curves[4].knots;
    let f = |k: &str| finite_number(&r[k]).unwrap_or(f64::NAN);
    let (t_ref, t_min, t_max) = (f("T_ref"), f("T_min"), f("T_max"));
    if !(knots[0] <= t_min && t_min <= t_ref && t_ref < t_max && t_max <= knots[knots.len() - 1])
        || t_min <= 0.0
    {
        return contract(
            "tabulated validity interval must be contained in the data without extrapolation",
        );
    }
    for key in ["E", "yield_stress", "k", "cp"] {
        let minimum = table[key].as_array().map_or(f64::NAN, |a| {
            a.iter()
                .filter_map(Value::as_f64)
                .fold(f64::INFINITY, f64::min)
        });
        if minimum <= 0.0 {
            return contract(format!("tabulated {key} must be positive"));
        }
    }
    let mut scalars: Map<String, Value> = r
        .iter()
        .filter(|(k, _)| {
            k.as_str() != "temperature_table" && k.as_str() != "temperature_table_units"
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (slot, key) in TEMPERATURE_KEYS.iter().enumerate() {
        scalars.insert((*key).into(), json!(curves[slot].value(t_ref)));
    }
    let zeros: Map<String, Value> = TEMPERATURE_KEYS
        .iter()
        .map(|k| ((*k).to_string(), json!(0.0)))
        .collect();
    scalars.insert("temperature_slopes".into(), Value::Object(zeros));
    let validated = validate_material(&Value::Object(scalars))?;
    let mut m = SolidMaterial::from_validated(r.clone());
    m.values = MATERIAL_KEYS.map(|k| finite_number(&validated[k]).unwrap_or(f64::NAN));
    m.slopes = [0.0; 5];
    m.tables = Some(Arc::new(curves));
    Ok(m)
}

fn real_matrix(v: &Value) -> Option<Vec<Vec<f64>>> {
    v.as_array()?
        .iter()
        .map(|row| {
            row.as_array()
                .and_then(|r| r.iter().map(finite_number).collect::<Option<Vec<f64>>>())
        })
        .collect()
}

fn validate_chaboche(raw: &Value) -> Result<SolidMaterial, CaeError> {
    let Some(r) = raw
        .as_object()
        .filter(|r| r.contains_key("kinematic_hardening"))
    else {
        return contract("explicit kinematic_hardening temperature table required");
    };
    let base: Map<String, Value> = r
        .iter()
        .filter(|(k, _)| k.as_str() != "kinematic_hardening")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut m = validate_table(&Value::Object(base))?;
    let h = r["kinematic_hardening"].as_object();
    let h_ok = h.is_some_and(|h| {
        h.len() == KIN_KEYS.len()
            && KIN_KEYS.iter().all(|k| h.contains_key(*k))
            && h["interpolation"].as_str() == Some("pchip")
    });
    let Some(h) = h.filter(|_| h_ok) else {
        return contract(
            "kinematic_hardening requires temperature_K, C_Pa, gamma, provenance, interpolation=pchip",
        );
    };
    if !h["provenance"]
        .as_str()
        .is_some_and(|p| !p.trim().is_empty())
    {
        return contract("hardening provenance required");
    }
    let t: Vec<f64> = match h["temperature_K"]
        .as_array()
        .and_then(|a| a.iter().map(finite_number).collect())
    {
        Some(t) => t,
        None => {
            return contract(
                "hardening temperature_K requires finite real numbers, not strings or booleans",
            );
        }
    };
    let Some(c) = real_matrix(&h["C_Pa"]) else {
        return contract("hardening C_Pa requires finite real numbers, not strings or booleans");
    };
    let Some(g) = real_matrix(&h["gamma"]) else {
        return contract("hardening gamma requires finite real numbers, not strings or booleans");
    };
    if t.len() < 2 || t.windows(2).any(|w| w[1] - w[0] <= 0.0) {
        return contract("strictly increasing finite hardening temperatures required");
    }
    let branches = c.first().map_or(0, Vec::len);
    let rectangular = |a: &Vec<Vec<f64>>| a.iter().all(|row| row.len() == branches);
    let nonneg = |a: &Vec<Vec<f64>>| a.iter().flatten().all(|v| *v >= 0.0);
    if c.len() != t.len()
        || !rectangular(&c)
        || !(1..=8).contains(&branches)
        || g.len() != c.len()
        || !rectangular(&g)
        || !nonneg(&c)
        || !nonneg(&g)
    {
        return contract(
            "C_Pa and gamma require matching nonnegative finite temperature-by-branch arrays",
        );
    }
    if !(t[0] <= m.t_min && m.t_min <= m.t_max && m.t_max <= t[t.len() - 1]) {
        return contract("hardening data must cover declared validity; no extrapolation");
    }
    #[allow(clippy::float_cmp)]
    if m.values[idx::H_ISO] != 0.0 || m.values[idx::H_KIN] != 0.0 {
        return contract("nonlinear kinematic law cannot silently add legacy H_iso/H_kin");
    }
    let mut kin = Vec::with_capacity(branches);
    for j in 0..branches {
        let cj: Vec<f64> = c.iter().map(|row| row[j]).collect();
        let gj: Vec<f64> = g.iter().map(|row| row[j]).collect();
        kin.push((
            PropertyCurve::new(t.clone(), &cj)?,
            PropertyCurve::new(t.clone(), &gj)?,
        ));
    }
    m.raw.clone_from(r);
    m.kinematic = Some(Arc::new(kin));
    Ok(m)
}

impl MaterialLaw {
    pub fn coefficients<S: Scalar>(
        a: &SolidMaterial,
        b: &SolidMaterial,
        mix: S,
    ) -> ThermoelasticCoefficients<S> {
        let w = -mix + 1.0;
        let lin = |x: f64, y: f64| w * x + mix * y;
        let e = lin(a.values[idx::E], b.values[idx::E]);
        let nu = lin(a.values[idx::NU], b.values[idx::NU]);
        let g = e / ((nu + 1.0) * 2.0);
        let k = e / ((-(nu * 2.0) + 1.0) * 3.0);
        let alpha = lin(a.values[idx::ALPHA], b.values[idx::ALPHA]);
        let capacity = lin(
            a.constant_strain_capacity.unwrap_or(f64::NAN),
            b.constant_strain_capacity.unwrap_or(f64::NAN),
        );
        ThermoelasticCoefficients {
            g,
            k,
            beta: k * 3.0 * alpha,
            capacity,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn entropy_storage_increment_from_delta<S: Scalar>(
        a: &SolidMaterial,
        b: &SolidMaterial,
        mix: S,
        density: S,
        stiffness: S,
        previous: &[S; 4],
        delta: &[S; 4],
        strain: &Mandel<S>,
        previous_strain: &Mandel<S>,
    ) -> [S; 4] {
        let c = Self::coefficients(a, b, mix);
        let trace_increment = mandel::trace(&mandel::sub(strain, previous_strain));
        std::array::from_fn(|i| {
            (previous[i] + delta[i])
                * (density * c.capacity * (delta[i] / previous[i]).ln_1p()
                    + stiffness * c.beta * trace_increment)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn entropy_storage_increment<S: Scalar>(
        a: &SolidMaterial,
        b: &SolidMaterial,
        mix: S,
        density: S,
        stiffness: S,
        t: &[S; 4],
        tp: &[S; 4],
        strain: &Mandel<S>,
        previous_strain: &Mandel<S>,
    ) -> [S; 4] {
        let c = Self::coefficients(a, b, mix);
        let trace_increment = mandel::trace(&mandel::sub(strain, previous_strain));
        std::array::from_fn(|i| {
            t[i] * (density * c.capacity * (t[i] / tp[i]).ln()
                + stiffness * c.beta * trace_increment)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn helmholtz<S: Scalar>(
        a: &SolidMaterial,
        b: &SolidMaterial,
        mix: S,
        density: S,
        stiffness: S,
        strain: &Mandel<S>,
        t: S,
    ) -> S {
        let c = Self::coefficients(a, b, mix);
        let trace = mandel::trace(strain);
        let deviator: Mandel<S> =
            std::array::from_fn(|i| strain[i] - trace * mandel::IDENTITY[i] / 3.0);
        let reference = a.t_ref;
        let mechanical = c.g * mandel::dot(&deviator, &deviator) + c.k * trace * trace * 0.5
            - c.beta * (t - reference) * trace;
        stiffness * mechanical - density * c.capacity * (t * (t / reference).ln() - t + reference)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn numerical_energy_defect<S: Scalar>(
        a: &SolidMaterial,
        b: &SolidMaterial,
        mix: S,
        density: S,
        stiffness: S,
        t: &[S; 4],
        tp: &[S; 4],
        strain: &Mandel<S>,
        previous_strain: &Mandel<S>,
    ) -> [S; 4] {
        let c = Self::coefficients(a, b, mix);
        let increment = mandel::sub(strain, previous_strain);
        let trace = mandel::trace(&increment);
        let deviator: Mandel<S> =
            std::array::from_fn(|i| increment[i] - trace * mandel::IDENTITY[i] / 3.0);
        let mechanical =
            stiffness * (c.g * mandel::dot(&deviator, &deviator) + c.k * trace * trace * 0.5);
        std::array::from_fn(|i| {
            mechanical + density * c.capacity * (t[i] * (t[i] / tp[i]).ln() - (t[i] - tp[i]))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn step_energy<S: Scalar>(
        a: &SolidMaterial,
        b: &SolidMaterial,
        mix: S,
        density: S,
        stiffness: S,
        t: &[S; 4],
        tp: &[S; 4],
        strain: &Mandel<S>,
        previous_strain: &Mandel<S>,
    ) -> BTreeMap<&'static str, [S; 4]> {
        let c = Self::coefficients(a, b, mix);
        let t_ref = a.t_ref;
        let internal = |e: &Mandel<S>, temperature: &[S; 4]| -> [S; 4] {
            let trace = mandel::trace(e);
            let deviator: Mandel<S> =
                std::array::from_fn(|i| e[i] - trace * mandel::IDENTITY[i] / 3.0);
            let mechanical = c.g * mandel::dot(&deviator, &deviator)
                + c.k * trace * trace * 0.5
                + c.beta * t_ref * trace;
            std::array::from_fn(|i| {
                stiffness * mechanical + density * c.capacity * (temperature[i] - t_ref)
            })
        };
        let trace = mandel::trace(strain);
        let deviator: Mandel<S> =
            std::array::from_fn(|i| strain[i] - trace * mandel::IDENTITY[i] / 3.0);
        let increment = mandel::sub(strain, previous_strain);
        let delta_trace = mandel::trace(&increment);
        let elastic_work =
            c.g * 2.0 * mandel::dot(&deviator, &increment) + c.k * trace * delta_trace;
        let work: [S; 4] = std::array::from_fn(|i| {
            stiffness * (elastic_work - c.beta * (t[i] - t_ref) * delta_trace)
        });
        let current = internal(strain, t);
        let previous = internal(previous_strain, tp);
        let defect = Self::numerical_energy_defect(
            a,
            b,
            mix,
            density,
            stiffness,
            t,
            tp,
            strain,
            previous_strain,
        );
        let entropy = Self::entropy_storage_increment(
            a,
            b,
            mix,
            density,
            stiffness,
            t,
            tp,
            strain,
            previous_strain,
        );
        let mut out = BTreeMap::new();
        out.insert("internal_energy_J_m3", current);
        out.insert(
            "internal_energy_increment_J_m3",
            std::array::from_fn(|i| current[i] - previous[i]),
        );
        out.insert("endpoint_stress_work_J_m3", work);
        out.insert("numerical_energy_defect_J_m3", defect);
        out.insert("entropy_thermal_storage_J_m3", entropy);
        out.insert(
            "first_law_identity_residual_J_m3",
            std::array::from_fn(|i| current[i] - previous[i] - work[i] + defect[i] - entropy[i]),
        );
        out
    }
}
