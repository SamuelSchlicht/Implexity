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


use ndarray::{ArrayD, IxDyn};
use serde_json::Value;
use crate::pyval::num;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Field {
    pub shape: Vec<usize>,
    pub values: Vec<f64>,
}

impl Field {
    #[must_use]
    pub fn new(shape: Vec<usize>, values: Vec<f64>) -> Self {
        Self { shape, values }
    }

    #[must_use]
    pub fn full(shape: &[usize], value: f64) -> Self {
        Self { shape: shape.to_vec(), values: vec![value; shape.iter().product()] }
    }

    #[must_use]
    pub fn from_array(a: &ArrayD<f64>) -> Self {
        Self { shape: a.shape().to_vec(), values: a.iter().copied().collect() }
    }

    #[must_use]
    pub fn to_array(&self) -> ArrayD<f64> {
        ArrayD::from_shape_vec(IxDyn(&self.shape), self.values.clone())
            .unwrap_or_else(|_| ArrayD::zeros(IxDyn(&[0])))
    }

    #[must_use]
    pub fn to_nested(&self) -> Value {
        nested(&self.shape, &self.values)
    }

    #[must_use]
    pub fn from_nested(value: &Value) -> Option<Self> {
        let mut shape = Vec::new();
        let mut cur = value;
        while let Value::Array(a) = cur {
            shape.push(a.len());
            match a.first() {
                Some(first) => cur = first,
                None => break,
            }
        }
        let mut values = Vec::new();
        if !collect(value, &shape, 0, &mut values) {
            return None;
        }
        Some(Self { shape, values })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

fn collect(value: &Value, shape: &[usize], depth: usize, out: &mut Vec<f64>) -> bool {
    if depth == shape.len() {
        return match value {
            Value::Number(n) => n.as_f64().is_some_and(|v| {
                out.push(v);
                true
            }),
            Value::Bool(b) => {
                out.push(if *b { 1.0 } else { 0.0 });
                true
            }
            _ => false,
        };
    }
    match value {
        Value::Array(a) if a.len() == shape[depth] => a.iter().all(|v| collect(v, shape, depth + 1, out)),
        _ => false,
    }
}

#[must_use]
pub fn nested(shape: &[usize], values: &[f64]) -> Value {
    if shape.is_empty() {
        return values.first().map_or(Value::Null, |v| num(*v));
    }
    let inner: usize = shape[1..].iter().product();
    Value::Array((0..shape[0]).map(|i| nested(&shape[1..], &values[i * inner..(i + 1) * inner])).collect())
}

#[must_use]
pub fn nested_bool(shape: &[usize], values: &[bool]) -> Value {
    if shape.is_empty() {
        return values.first().map_or(Value::Null, |v| Value::Bool(*v));
    }
    let inner: usize = shape[1..].iter().product();
    Value::Array(
        (0..shape[0]).map(|i| nested_bool(&shape[1..], &values[i * inner..(i + 1) * inner])).collect(),
    )
}

#[must_use]
pub fn bool_mask(value: &Value) -> Option<(Vec<usize>, Vec<bool>)> {
    Field::from_nested(value).map(|f| (f.shape, f.values.iter().map(|v| *v != 0.0).collect()))
}

#[must_use]
pub fn is_bool_array(value: &Value) -> bool {
    match value {
        Value::Bool(_) => true,
        Value::Array(a) => a.iter().all(is_bool_array),
        _ => false,
    }
}
