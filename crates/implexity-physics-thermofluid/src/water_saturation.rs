// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::any::Any;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_ad::{HyperDual, Scalar};
use implexity_core::CaeError;
use implexity_core::orchestration::{
    AddInAdapter, AddInCategory, AddInContract, AddInRegistry, ContractInput, Fidelity, PortSpec,
};
use implexity_core::packages::InstallContext;

pub const N: [f64; 10] = [
    1_167.052_145_276_7,
    -724_213.167_032_06,
    -17.073_846_940_092,
    12_020.824_702_470,
    -3_232_555.032_233_3,
    14.915_108_613_530,
    -4_823.265_736_159_1,
    405_113.405_420_57,
    -0.238_555_575_678_49,
    650.175_348_447_98,
];
pub const T_MIN_K: f64 = 273.15;
pub const T_MAX_K: f64 = 647.096;
pub const P_MIN_PA: f64 = 611.212_677_444;
pub const P_MAX_PA: f64 = 22.064e6;
pub const IMPLEMENTATION: &str = "implexity.physics_library.water_saturation.WaterSaturationIF97";
pub const SATURATION_INTERFACE: &str = "liquid_vapour_saturation";
pub const ADDIN_ID: &str = "iapws_if97_saturation";

#[must_use]
pub fn source() -> Value {
    json!({
        "organization": "IAPWS", "release": "R7-97(2012)", "equations": [30, 31],
        "coefficient_table": 34, "verification_tables": [35, 36],
        "url": "https://iapws.org/technical-guidance/release/IF97-Rev.download"
    })
}

#[must_use]
pub fn saturation_temperature<S: Scalar>(pressure_absolute_pa: S) -> S {
    let [n1, n2, n3, n4, n5, n6, n7, n8, n9, n10] = N;
    let beta = (pressure_absolute_pa / 1e6).powf(0.25);
    let e = beta * beta + beta * n3 + n6;
    let f = beta * n1 * beta + beta * n4 + n7;
    let g = beta * n2 * beta + beta * n5 + n8;
    let d = g * 2.0 / (-f - (f * f - e * 4.0 * g).sqrt());
    let s = d + n10;
    ((d + n10) - (s * s - (d * n10 + n9) * 4.0).sqrt()) * 0.5
}

#[must_use]
pub fn saturation_pressure<S: Scalar>(temperature_k: S) -> S {
    let [n1, n2, n3, n4, n5, n6, n7, n8, n9, n10] = N;
    let t = temperature_k;
    let theta = t + S::from_f64(n9) / (t - n10);
    let a = theta * theta + theta * n1 + n2;
    let b = theta * n3 * theta + theta * n4 + n5;
    let c = theta * n6 * theta + theta * n7 + n8;
    (c * 2.0 / (-b + (b * b - a * 4.0 * c).sqrt())).powf(4.0) * 1e6
}

pub trait SaturationLaw: Send + Sync {

    fn validate(&self, settings: &Value) -> Result<Value, CaeError>;

    fn pressure_check(&self, pressures: &[f64]) -> Result<(), CaeError>;

    fn temperature_check(&self, temperatures: &[f64]) -> Result<(), CaeError>;
    fn temperature_jet(&self, pressure: f64) -> (f64, f64, f64);
    fn pressure_jet(&self, temperature: f64) -> (f64, f64, f64);
}

#[must_use]
pub fn law_temperature<S: Scalar>(law: &dyn SaturationLaw, p: S) -> S {
    let (v, d, dd) = law.temperature_jet(p.value());
    p.chain(v, d, dd)
}

#[must_use]
pub fn law_pressure<S: Scalar>(law: &dyn SaturationLaw, t: S) -> S {
    let (v, d, dd) = law.pressure_jet(t.value());
    t.chain(v, d, dd)
}

pub struct SaturationInterface(pub Arc<dyn SaturationLaw>);


pub fn saturation_component(addins: &AddInRegistry, name: &str) -> Result<Arc<dyn SaturationLaw>, CaeError> {
    let row = addins.get(name)?;
    let law = row
        .adapter
        .as_ref()
        .filter(|a| a.component_kind().as_deref() == Some(SATURATION_INTERFACE))
        .and_then(|a| {
            a.interface(SATURATION_INTERFACE)
                .and_then(|i| i.downcast_ref::<SaturationInterface>())
                .map(|i| Arc::clone(&i.0))
        });
    law.ok_or_else(|| {
        CaeError::contract("liquid phase margin requires an active saturation constitutive component")
    })
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WaterSaturationIf97;

pub const LIMITATIONS: [&str; 1] =
    ["Equilibrium saturation line only; NOT CHF, DNBR, nucleation, two-phase flow, EOS or transport data."];

impl WaterSaturationIf97 {
    pub const COMPONENT_KIND: &'static str = SATURATION_INTERFACE;
    pub const SPECIES: &'static str = "water";

    #[must_use]
    pub fn runtime_support() -> Map<String, Value> {
        let v = json!({"status": "field_component", "data": "IAPWS-IF97 region 4 coefficients",
                       "history": false, "limitations": LIMITATIONS});
        v.as_object().cloned().unwrap_or_default()
    }


    pub fn evaluate(
        &self,
        pressure_absolute_pa: Option<&[f64]>,
        temperature_k: Option<&[f64]>,
    ) -> Result<Vec<f64>, CaeError> {
        match (pressure_absolute_pa, temperature_k) {
            (Some(p), None) => {
                self.pressure_check(p)?;
                Ok(p.iter().map(|v| saturation_temperature(*v)).collect())
            }
            (None, Some(t)) => {
                self.temperature_check(t)?;
                Ok(t.iter().map(|v| saturation_pressure(*v)).collect())
            }
            _ => Err(CaeError::contract("supply exactly one saturation-line argument")),
        }
    }
}

impl SaturationLaw for WaterSaturationIf97 {
    fn validate(&self, settings: &Value) -> Result<Value, CaeError> {
        if *settings != json!({"species": "water"}) {
            return Err(CaeError::contract(
                "IAPWS region 4 requires species=water, with no calibration or override of coefficients",
            ));
        }
        Ok(settings.clone())
    }

    fn pressure_check(&self, pressures: &[f64]) -> Result<(), CaeError> {
        if pressures.iter().any(|p| !p.is_finite() || *p < P_MIN_PA || *p > P_MAX_PA) {
            return Err(CaeError::contract(
                "IAPWS region 4 requires absolute pressure in [611.212677444, 22064000] Pa; no clipping/extrapolation",
            ));
        }
        Ok(())
    }

    fn temperature_check(&self, temperatures: &[f64]) -> Result<(), CaeError> {
        if temperatures.iter().any(|t| !t.is_finite() || *t < T_MIN_K || *t > T_MAX_K) {
            return Err(CaeError::contract(
                "IAPWS region 4 saturation-pressure input must be in [273.15, 647.096] K",
            ));
        }
        Ok(())
    }

    fn temperature_jet(&self, pressure: f64) -> (f64, f64, f64) {
        let h = saturation_temperature(HyperDual::new(pressure, 1.0, 1.0, 0.0));
        (h.re, h.e1, h.e12)
    }

    fn pressure_jet(&self, temperature: f64) -> (f64, f64, f64) {
        let h = saturation_pressure(HyperDual::new(temperature, 1.0, 1.0, 0.0));
        (h.re, h.e1, h.e12)
    }
}

impl AddInAdapter for WaterSaturationIf97 {
    fn implementation(&self) -> String {
        IMPLEMENTATION.into()
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(Self::runtime_support())
    }
    fn component_kind(&self) -> Option<String> {
        Some(Self::COMPONENT_KIND.into())
    }
    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        static LAW: std::sync::LazyLock<SaturationInterface> =
            std::sync::LazyLock::new(|| SaturationInterface(Arc::new(WaterSaturationIf97)));
        (name == SATURATION_INTERFACE).then(|| &*LAW as &(dyn Any + Send + Sync))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[must_use]
pub fn contract() -> AddInContract {
    let mut c = AddInContract::new(ADDIN_ID);
    c.category = AddInCategory::Constitutive;
    let mut t = PortSpec::new("liquid_vapour_saturation_temperature");
    t.unit = "K".into();
    let mut p = PortSpec::new("liquid_vapour_saturation_pressure");
    p.unit = "Pa".into();
    c.provides = vec![t, p];
    c.fidelity = Fidelity::High;
    c.notes = vec![
        "IAPWS R7-97(2012) region 4 equations 30/31; absolute pressure in Pa.".into(),
        "Does not claim critical heat flux or two-phase equations.".into(),
    ];
    c
}


pub fn register(ctx: &InstallContext<'_>) -> Result<AddInContract, CaeError> {
    ctx.register_addin(ContractInput::Typed(Box::new(contract())), Some(Arc::new(WaterSaturationIf97)))
}

