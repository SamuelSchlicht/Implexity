// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use serde_json::Value;

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_core::orchestration::AddInRegistry;

pub const EXCHANGE_INTERFACE: &str = "thermal_exchange";


pub fn exchange_parameter(parameters: &Value, key: &str) -> Result<f64, CaeError> {
    let ok_keys = parameters
        .as_object()
        .is_some_and(|m| m.len() == 2 && m.contains_key(key) && m.contains_key("provenance"));
    if !ok_keys {
        return Err(CaeError::contract(format!("thermal exchange requires {key} and provenance")));
    }
    let value = match &parameters[key] {
        Value::Number(n) => n.as_f64().filter(|v: &f64| v.is_finite()),
        _ => None,
    };
    let Some(value) = value else {
        return Err(CaeError::contract(format!("{key} must be a finite real numeric scalar")));
    };
    match &parameters["provenance"] {
        Value::String(s) if !s.trim().is_empty() => Ok(value),
        _ => Err(CaeError::contract("thermal exchange requires textual parameter provenance")),
    }
}

pub trait ThermalExchangeLaw: Send + Sync {

    fn validate(&self, parameters: &Value) -> Result<Value, CaeError>;
    fn flux_jet(&self, temperature_a: f64, temperature_b: f64, parameters: &Value) -> [f64; 6];
}

#[must_use]
pub fn exchange_flux<S: Scalar>(law: &dyn ThermalExchangeLaw, ta: S, tb: S, parameters: &Value) -> S {
    let [f, fa, fb, faa, fab, fbb] = law.flux_jet(ta.value(), tb.value(), parameters);
    S::chain2(ta, tb, f, fa, fb, faa, fab, fbb)
}

pub struct ExchangeInterface(pub Arc<dyn ThermalExchangeLaw>);


pub fn exchange_component(
    addins: &AddInRegistry,
    name: &str,
) -> Result<Arc<dyn ThermalExchangeLaw>, CaeError> {
    let row = addins.get(name)?;
    row.adapter
        .as_ref()
        .and_then(|a| a.interface(EXCHANGE_INTERFACE))
        .and_then(|i| i.downcast_ref::<ExchangeInterface>())
        .map(|i| Arc::clone(&i.0))
        .ok_or_else(|| {
            CaeError::contract(format!("{name}: inactive or incompatible thermal exchange component"))
        })
}
