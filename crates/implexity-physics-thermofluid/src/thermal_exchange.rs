// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_core::CaeError;
use implexity_core::orchestration::{
    AddInAdapter, AddInCategory, AddInContract, ContractInput, Fidelity, PortSpec,
};
use implexity_core::packages::InstallContext;

pub const STEFAN_BOLTZMANN: f64 = 5.670_374_419_184_431_4e-8;
pub use implexity_physics_base::thermal_exchange_law::{
    EXCHANGE_INTERFACE, ExchangeInterface, ThermalExchangeLaw, exchange_component, exchange_flux,
    exchange_parameter,
};

fn runtime_support(limitation: &str) -> Map<String, Value> {
    json!({"status": "field_component", "history": false, "data": "user_required", "limitations": [limitation]})
        .as_object()
        .cloned()
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NewtonExchange;

pub const NEWTON_LIMITATION: &str =
    "Authored positive film/contact conductance; does not compute coolant flow or boiling.";
pub const GRAY_LIMITATION: &str = "Diffuse-gray exchange with an authored effective exchange factor; no view-factor solver, participating medium or ray tracing.";

impl ThermalExchangeLaw for NewtonExchange {
    fn validate(&self, parameters: &Value) -> Result<Value, CaeError> {
        let h = exchange_parameter(parameters, "coefficient_W_m2K")?;
        if h <= 0.0 {
            return Err(CaeError::contract(
                "finite positive heat-transfer coefficient and provenance required",
            ));
        }
        let mut out = parameters.as_object().cloned().unwrap_or_default();
        out.insert("coefficient_W_m2K".into(), json!(h));
        Ok(Value::Object(out))
    }
    fn flux_jet(&self, ta: f64, tb: f64, parameters: &Value) -> [f64; 6] {
        let h = parameters["coefficient_W_m2K"].as_f64().unwrap_or(f64::NAN);
        [h * (ta - tb), h, -h, 0.0, 0.0, 0.0]
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GrayRadiation;

impl ThermalExchangeLaw for GrayRadiation {
    fn validate(&self, parameters: &Value) -> Result<Value, CaeError> {
        let f = exchange_parameter(parameters, "effective_exchange_factor")?;
        if !(0.0 < f && f <= 1.0) {
            return Err(CaeError::contract("effective exchange factor must lie in (0,1] with provenance"));
        }
        let mut out = parameters.as_object().cloned().unwrap_or_default();
        out.insert("effective_exchange_factor".into(), json!(f));
        Ok(Value::Object(out))
    }
    fn flux_jet(&self, ta: f64, tb: f64, parameters: &Value) -> [f64; 6] {
        let c = STEFAN_BOLTZMANN * parameters["effective_exchange_factor"].as_f64().unwrap_or(f64::NAN);
        let q = c * (ta - tb) * (ta + tb) * (ta * ta + tb * tb);
        [q, 4.0 * c * ta.powi(3), -4.0 * c * tb.powi(3), 12.0 * c * ta * ta, 0.0, -12.0 * c * tb * tb]
    }
}

macro_rules! exchange_adapter {
    ($ty:ty, $path:literal, $limitation:expr) => {
        impl AddInAdapter for $ty {
            fn implementation(&self) -> String {
                $path.into()
            }
            fn runtime_support(&self) -> Option<Map<String, Value>> {
                Some(runtime_support($limitation))
            }
            fn component_kind(&self) -> Option<String> {
                Some(EXCHANGE_INTERFACE.into())
            }
            fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
                static LAW: std::sync::LazyLock<ExchangeInterface> =
                    std::sync::LazyLock::new(|| ExchangeInterface(Arc::new(<$ty>::default())));
                (name == EXCHANGE_INTERFACE).then(|| &*LAW as &(dyn Any + Send + Sync))
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }
    };
}

exchange_adapter!(
    NewtonExchange,
    "implexity.physics_library.thermal_exchange.NewtonExchange",
    NEWTON_LIMITATION
);
exchange_adapter!(GrayRadiation, "implexity.physics_library.thermal_exchange.GrayRadiation", GRAY_LIMITATION);

fn contract(id: &str, limitation: &str) -> AddInContract {
    let mut c = AddInContract::new(id);
    c.category = AddInCategory::Constitutive;
    let mut port = PortSpec::new("reciprocal_thermal_surface_flux");
    port.unit = "W/m^2".into();
    c.provides = vec![port];
    c.fidelity = Fidelity::Intermediate;
    c.direct_topology_dependence = Some(false);
    c.notes = vec![limitation.to_string()];
    c
}


pub fn register_components(ctx: &InstallContext<'_>) -> Result<(), CaeError> {
    ctx.register_addin(
        ContractInput::Typed(Box::new(contract("newton_exchange", NEWTON_LIMITATION))),
        Some(Arc::new(NewtonExchange)),
    )?;
    ctx.register_addin(
        ContractInput::Typed(Box::new(contract("gray_radiation", GRAY_LIMITATION))),
        Some(Arc::new(GrayRadiation)),
    )?;
    Ok(())
}
