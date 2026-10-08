// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::any::Any;
use std::sync::Arc;

use serde_json::{Value, json};

use implexity_core::contributions::{ContributionRegistry, ContributionValue};

use crate::error::{AResult, AuthoringError};

pub const KIND: &str = "implicit_physics";

#[must_use]
pub fn binding_error(problems: Vec<String>) -> AuthoringError {
    AuthoringError::problems("BindingError", problems)
}

pub type Warm = Arc<dyn Any + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
pub struct Occupancy {
    pub shape: [usize; 3],
    pub values: Vec<f64>,
}

pub trait PreparedPhysics: Send {
    fn report(&self) -> Value;
    fn l0(&self) -> Option<f64>;
    fn refs(&self) -> Value;
    fn penalties_at_start(&self) -> Value {
        Value::Null
    }
    fn start_state(&self) -> Option<Warm> {
        None
    }
    fn warm0(&self) -> Option<Warm> {
        None
    }

    fn probe(&mut self, dm0: &Occupancy) -> AResult<Value>;

    fn build_objective(&mut self, block: &Value) -> AResult<Value>;

    fn value(&mut self, dm: &Occupancy, warm: Option<&Warm>) -> AResult<(f64, Value, Option<Warm>)>;

    fn value_and_grad(
        &mut self,
        dm: &Occupancy,
        warm: Option<&Warm>,
    ) -> AResult<(f64, Vec<f64>, Value, Option<Warm>)>;

    fn state(&mut self, dm: &Occupancy, warm: Option<&Warm>) -> AResult<Value>;

    fn calibrate(&mut self, dm0: &Occupancy) -> AResult<()>;

    fn restore(&mut self, l0: f64, refs: &Value, extras: &Value) -> AResult<()>;
    fn term_row(&self, aux: &Value) -> Value;
    fn extras(&self) -> Value {
        json!({})
    }

    fn response_values_and_grads(
        &mut self,
        dm: &Occupancy,
        warm: Option<&Warm>,
        names: &[String],
    ) -> AResult<Option<(Vec<f64>, Vec<Vec<f64>>)>> {
        let _ = (dm, warm, names);
        Ok(None)
    }
    fn warm_to_arrays(
        &self,
        warm: &Warm,
    ) -> Option<std::collections::BTreeMap<String, (Vec<usize>, Vec<f64>)>> {
        let _ = warm;
        None
    }

    fn warm_from_arrays(
        &self,
        arrays: &std::collections::BTreeMap<String, (Vec<usize>, Vec<f64>)>,
    ) -> AResult<Option<Warm>> {
        let _ = arrays;
        Ok(None)
    }
}

pub trait ImplicitPhysics: Send + Sync {
    fn name(&self) -> &str;
    fn provider(&self) -> &str;
    fn label(&self) -> &str {
        self.name()
    }
    fn implementation(&self) -> &str;
    fn row_quantities(&self) -> Vec<[String; 4]> {
        Vec::new()
    }
    fn responses(&self) -> Vec<[String; 4]> {
        Vec::new()
    }

    fn validate_case(&self, case: &Value) -> AResult<(Value, Vec<String>)>;

    fn analysis_box(&self, norm: &Value) -> AResult<Value>;

    fn effective_case(&self, norm: &Value, grid: &[usize]) -> AResult<Value>;
    fn validate_objective(&self, block: &Value, problems: &mut Vec<String>) -> Value;

    fn describe_case(&self, norm: &Value) -> AResult<Value> {
        let b = self.analysis_box(norm)?;
        Ok(json!({"name": b.get("name").cloned().unwrap_or(Value::Null),
            "grid": b.get("grid").cloned().unwrap_or(Value::Null),
            "h_mm": b.get("h_mm").cloned().unwrap_or(Value::Null),
            "domain_mm": b.get("domain_mm").cloned().unwrap_or(Value::Null)}))
    }
    fn record_case(&self, norm: &Value, objective_block: &Value) -> Value {
        let mut out = norm.clone();
        if crate::py::truthy(objective_block)
            && let Some(m) = out.as_object_mut()
        {
            m.insert("objective".into(), objective_block.clone());
        }
        out
    }
    fn current_case(&self, _svc: &dyn Any) -> Option<Value> {
        None
    }
    fn record_objective(
        &self,
        _case: &Value,
        _objective_meta: &Value,
        _last_row: &Value,
        _refs: &Value,
        _l0: Option<f64>,
    ) -> Option<Value> {
        None
    }
    fn record_fidelity(&self, _summary: &Value, _case: &Value) -> Vec<Value> {
        Vec::new()
    }

    fn prepare(
        &self,
        spec: &Value,
        log: Option<&(dyn Fn(&str) + Send + Sync)>,
    ) -> AResult<Box<dyn PreparedPhysics>>;
}

pub struct BindingHandle(pub Arc<dyn ImplicitPhysics>);


pub fn register(
    reg: &ContributionRegistry,
    binding: Arc<dyn ImplicitPhysics>,
    owner_id: &str,
) -> AResult<()> {
    if binding.name().is_empty() {
        return Err(crate::py::value_error("an Optimize-node binding is a named ImplicitPhysics"));
    }
    let name = binding.name().to_string();
    let implementation = binding.implementation().to_string();
    let identity = Arc::as_ptr(&binding).cast::<()>() as usize;
    let value = ContributionValue::with_identity(Arc::new(BindingHandle(binding)), identity, implementation);
    reg.register(KIND, &name, value, owner_id).map_err(|e| crate::py::value_error(e.0))?;
    Ok(())
}

#[must_use]
pub fn bindings(reg: &ContributionRegistry) -> Vec<(String, Arc<dyn ImplicitPhysics>)> {
    reg.entries(KIND)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.downcast::<BindingHandle>().map(|h| (k, Arc::clone(&h.0))))
        .collect()
}

#[must_use]
pub fn for_provider(reg: &ContributionRegistry, provider: &str) -> Option<Arc<dyn ImplicitPhysics>> {
    bindings(reg).into_iter().find(|(_, b)| b.provider() == provider).map(|(_, b)| b)
}


pub fn resolve(reg: &ContributionRegistry, name: Option<&str>) -> AResult<Arc<dyn ImplicitPhysics>> {
    let active = bindings(reg);
    let mut names: Vec<String> = active.iter().map(|(k, _)| k.clone()).collect();
    names.sort();
    let listed = if names.is_empty() { "none".to_string() } else { implexity_core::pyobj::list_repr(&names) };
    if let Some(n) = name.filter(|n| !n.is_empty()) {
        return active.into_iter().find(|(k, _)| k == n).map(|(_, b)| b).ok_or_else(|| {
            binding_error(vec![format!(
                "NO_PHYSICS_LOADED: no active physics package binds Optimize nodes to backend {}; active: {listed}",
                crate::py::repr(&Value::from(n))
            )])
        });
    }
    match active.len() {
        1 => Ok(Arc::clone(&active[0].1)),
        0 => Err(binding_error(vec![
            "NO_PHYSICS_LOADED: load a physics package that binds Optimize nodes to a physics solve before optimising or analysing".into(),
        ])),
        _ => Err(binding_error(vec![format!(
            "several physics packages bind Optimize nodes ({}); name one with the 'physics' setting",
            implexity_core::pyobj::list_repr(&names)
        )])),
    }
}
