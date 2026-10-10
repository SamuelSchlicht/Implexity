// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END


mod closures;
mod fields;

pub use closures::*;
pub use fields::*;

use serde_json::{Map, Value};

use implexity_ad::Scalar;
use implexity_core::pyobj::PyNum;

use crate::array::Tensor;
use crate::model_errors::{PhysicsError, PhysicsResult};

pub const R_GAS: f64 = 8.314_462_618_153_24;
pub const FARADAY: f64 = 96_485.332_12;

#[derive(Debug, Clone, PartialEq)]
pub enum ModelValue<S> {
    Array(Tensor<S>),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelOutputs<S> {
    entries: Vec<(String, ModelValue<S>)>,
}

impl<S: Scalar> ModelOutputs<S> {
    #[must_use]
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn set(&mut self, key: &str, value: Tensor<S>) {
        self.set_value(key, ModelValue::Array(value));
    }

    pub fn set_scalar(&mut self, key: &str, value: S) {
        self.set(key, Tensor::scalar(value));
    }

    pub fn set_text(&mut self, key: &str, text: &str) {
        self.set_value(key, ModelValue::Text(text.into()));
    }

    fn set_value(&mut self, key: &str, value: ModelValue<S>) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            self.entries.push((key.into(), value));
        }
    }

    #[must_use]
    pub fn with(mut self, key: &str, value: Tensor<S>) -> Self {
        self.set(key, value);
        self
    }

    #[must_use]
    pub fn with_scalar(mut self, key: &str, value: S) -> Self {
        self.set_scalar(key, value);
        self
    }

    pub fn extend(&mut self, other: Self) {
        for (k, v) in other.entries {
            self.set_value(&k, v);
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Tensor<S>> {
        self.entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
            ModelValue::Array(t) => Some(t),
            ModelValue::Text(_) => None,
        })
    }

    #[must_use]
    pub fn get_value(&self, key: &str) -> Option<&ModelValue<S>> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }


    pub fn require(&self, key: &str) -> PhysicsResult<&Tensor<S>> {
        self.get(key).ok_or_else(|| PhysicsError::Key(implexity_core::py_repr::repr_str(key)))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ModelValue<S>)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[must_use]
pub fn outputs_to_json(outputs: &ModelOutputs<f64>) -> Value {
    let mut m = Map::new();
    for (k, v) in outputs.iter() {
        m.insert(
            k.to_string(),
            match v {
                ModelValue::Array(t) => t.to_json(),
                ModelValue::Text(s) => Value::String(s.clone()),
            },
        );
    }
    Value::Object(m)
}

pub fn softplus<S: Scalar>(x: S, width: f64) -> S {
    (x / width).logaddexp(S::zero()) * width
}

pub fn sigmoid<S: Scalar>(x: S) -> S {
    ((x * 0.5).tanh() + 1.0) * 0.5
}

pub fn pow_num<S: Scalar>(x: S, p: PyNum) -> S {
    match p {
        PyNum::Int(n) => x.powi(i32::try_from(n).unwrap_or(i32::MAX)),
        PyNum::Float(f) => x.powf(f),
    }
}


pub fn valid_temperature<S: Scalar>(t: &Tensor<S>, tmin: f64, tmax: f64) -> PhysicsResult<S> {
    let lo = t.map(|x| x - tmin).min()?;
    let hi = t.map(|x| -x + tmax).min()?;
    Ok(lo.minimum(hi))
}

pub struct Kwargs<'a> {
    map: &'a Map<String, Value>,
}

impl<'a> Kwargs<'a> {
    #[must_use]
    pub fn new(map: &'a Map<String, Value>) -> Self {
        Self { map }
    }

    #[must_use]
    pub fn raw(&self, name: &str) -> Option<&'a Value> {
        self.map.get(name)
    }


    pub fn num(&self, name: &str, default: Option<PyNum>) -> PhysicsResult<PyNum> {
        match self.map.get(name) {
            None => {
                default.ok_or_else(|| PhysicsError::value(format!("missing numerical authoring ['{name}']")))
            }
            Some(Value::Bool(b)) => Ok(PyNum::Int(i64::from(*b))),
            Some(v) => PyNum::from_value(v).ok_or_else(|| {
                PhysicsError::Type(format!(
                    "must be real number, not {}",
                    implexity_core::pyobj::type_name(v)
                ))
            }),
        }
    }


    pub fn f64(&self, name: &str, default: Option<f64>) -> PhysicsResult<f64> {
        self.num(name, default.map(PyNum::Float)).map(PyNum::as_f64)
    }


    pub fn text(&self, name: &str, default: Option<&str>) -> PhysicsResult<String> {
        match self.map.get(name) {
            None => default
                .map(str::to_string)
                .ok_or_else(|| PhysicsError::value(format!("missing numerical authoring ['{name}']"))),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(other) => Ok(implexity_core::pyobj::py_str(other)),
        }
    }

    #[must_use]
    pub fn any(&self, name: &str) -> Option<Value> {
        self.map.get(name).filter(|v| !v.is_null()).cloned()
    }
}

pub trait RuntimeModel: Sized {
    const CLASS: &'static str;
    const PARAMS: &'static [(&'static str, bool)];


    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self>;
}


pub fn construct<M: RuntimeModel>(addin_id: &str, auth: &Map<String, Value>) -> PhysicsResult<M> {
    let kwargs: Map<String, Value> = auth
        .iter()
        .filter(|(k, _)| M::PARAMS.iter().any(|(p, _)| p == k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let missing: Vec<String> = M::PARAMS
        .iter()
        .filter(|(p, has_default)| !has_default && !kwargs.contains_key(*p))
        .map(|(p, _)| implexity_core::py_repr::repr_str(p))
        .collect();
    if !missing.is_empty() {
        return Err(PhysicsError::value(format!(
            "{addin_id}: missing numerical authoring [{}]",
            missing.join(", ")
        )));
    }
    M::build(&Kwargs::new(&kwargs))
}

#[must_use]
pub fn all_finite(values: &[f64]) -> bool {
    values.iter().all(Scalar::is_finite)
}


pub fn require_finite<S: Scalar>(t: &Tensor<S>, message: &str) -> PhysicsResult<()> {
    if t.data().iter().all(Scalar::is_finite) { Ok(()) } else { Err(PhysicsError::value(message)) }
}
