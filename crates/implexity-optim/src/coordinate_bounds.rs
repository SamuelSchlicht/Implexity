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


use implexity_core::contracts::{CaeProvider, ProviderProblem};
use implexity_core::numeric_contract::real_array;
use implexity_core::{CaeError, CaeResult};
use ndarray::{ArrayD, Zip};
use serde_json::{Map, Value};

use crate::design_state::full_like;
use crate::numeric::{array_to_value, float_value, shape_repr};
use crate::provider_ops::{DesignOp, design_operations};

#[derive(Debug, Clone, PartialEq)]
pub enum Bound {
    Scalar(f64),
    Array(ArrayD<f64>),
}

impl Bound {
    #[must_use]
    pub fn to_wire(&self) -> Value {
        match self {
            Self::Scalar(s) => float_value(*s),
            Self::Array(a) => array_to_value(a),
        }
    }

    #[must_use]
    pub fn is_array(&self) -> bool {
        matches!(self, Self::Array(_))
    }

    #[must_use]
    pub fn broadcast(&self, shape: &[usize]) -> ArrayD<f64> {
        match self {
            Self::Scalar(s) => ArrayD::from_elem(ndarray::IxDyn(shape), *s),
            Self::Array(a) => a.clone(),
        }
    }
}

fn numeric_kind(raw: &Value) -> bool {
    match raw {
        Value::Number(_) => true,
        Value::Array(items) => items.iter().all(numeric_kind),
        _ => false,
    }
}


pub fn coordinate_bounds(
    value: &ArrayD<f64>,
    lower: &Value,
    upper: &Value,
    name: &str,
) -> CaeResult<(Bound, Bound)> {
    for raw in [lower, upper] {
        if !numeric_kind(raw) {
            return Err(CaeError::contract(format!("{name}: bounds require real numeric values")));
        }
    }
    let lo = full_like(value, lower, "lower", name)?;
    let hi = full_like(value, upper, "upper", name)?;
    let valid = lo.iter().zip(hi.iter()).all(|(l, h)| l < h && (h - l).is_finite());
    if !valid {
        return Err(CaeError::contract(format!("{name}: bounds need finite positive spans")));
    }
    let wrap = |raw: &Value, arr: ArrayD<f64>| match raw {
        Value::Number(n) => Bound::Scalar(n.as_f64().unwrap_or(f64::NAN)),
        _ => Bound::Array(arr),
    };
    Ok((wrap(lower, lo), wrap(upper, hi)))
}

#[must_use]
pub fn array_bounds(lo: &Bound, hi: &Bound) -> bool {
    lo.is_array() || hi.is_array()
}

#[must_use]
pub fn same_bounds(shape: &[usize], left: &Bound, right: &Bound) -> bool {
    left.broadcast(shape) == right.broadcast(shape)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoxMasks {
    pub fixed_solid: Option<ArrayD<f64>>,
    pub fixed_void: Option<ArrayD<f64>>,
    pub preserve: Option<ArrayD<f64>>,
    pub designable: Option<ArrayD<f64>>,
}

impl BoxMasks {

    pub fn from_value(masks: &Map<String, Value>) -> CaeResult<Self> {
        let get = |k: &str| -> CaeResult<Option<ArrayD<f64>>> {
            match masks.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => real_array(v, k).map(Some),
            }
        };
        Ok(Self {
            fixed_solid: get("fixed_solid")?,
            fixed_void: get("fixed_void")?,
            preserve: get("preserve")?,
            designable: get("designable")?,
        })
    }
}


pub fn normalise_mask(
    value: Option<&ArrayD<f64>>,
    shape: &[usize],
    name: &str,
) -> CaeResult<Option<ArrayD<bool>>> {
    let Some(a) = value else { return Ok(None) };
    if a.shape() != shape {
        return Err(CaeError::contract(format!(
            "{name} shape {} differs from model:control {}",
            shape_repr(a.shape()),
            shape_repr(shape)
        )));
    }
    if a.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract(format!("{name} contains non-finite values")));
    }
    Ok(Some(a.mapv(|v| v > 0.5)))
}


pub fn project_array_box(
    x: &ArrayD<f64>,
    lo: &Bound,
    hi: &Bound,
    masks: &BoxMasks,
    initial: &ArrayD<f64>,
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
) -> CaeResult<ArrayD<f64>> {
    let shape = x.shape().to_vec();
    let lo = lo.broadcast(&shape);
    let hi = hi.broadcast(&shape);
    let solid = normalise_mask(masks.fixed_solid.as_ref(), &shape, "fixed_solid")?;
    let void = normalise_mask(masks.fixed_void.as_ref(), &shape, "fixed_void")?;
    let preserve = normalise_mask(masks.preserve.as_ref(), &shape, "preserve")?;
    let designable = normalise_mask(masks.designable.as_ref(), &shape, "designable")?;
    if let (Some(s), Some(v)) = (&solid, &void)
        && s.iter().zip(v.iter()).any(|(a, b)| *a && *b)
    {
        return Err(CaeError::contract("fixed masks overlap"));
    }
    let enforce = |v: &ArrayD<f64>| -> ArrayD<f64> {
        let mut v = v.clone();
        Zip::from(&mut v).and(&lo).and(&hi).for_each(|v, l, h| *v = v.max(*l).min(*h));
        if let Some(d) = &designable {
            Zip::from(&mut v).and(d).and(initial).for_each(|v, d, i| {
                if !*d {
                    *v = *i;
                }
            });
        }
        if let Some(p) = &preserve {
            Zip::from(&mut v).and(p).and(initial).for_each(|v, p, i| {
                if *p {
                    *v = *i;
                }
            });
        }
        if let Some(m) = &void {
            Zip::from(&mut v).and(m).and(&lo).for_each(|v, m, l| {
                if *m {
                    *v = *l;
                }
            });
        }
        if let Some(m) = &solid {
            Zip::from(&mut v).and(m).and(&hi).for_each(|v, m, h| {
                if *m {
                    *v = *h;
                }
            });
        }
        v
    };

    let mut y = x.clone();
    Zip::from(&mut y).and(&lo).and(&hi).for_each(|v, l, h| *v = v.max(*l).min(*h));
    let mut y = enforce(&y);
    if let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::ProjectTopology)) {
        let raw = ops.project_topology(problem, &y, initial)?;
        if raw.shape() != shape.as_slice() || raw.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("invalid provider projection"));
        }
        y = enforce(&raw);
        let check = ops.project_topology(problem, &y, initial)?;
        if check.shape() != shape.as_slice() || check.iter().any(|v| !v.is_finite()) || check != y {
            return Err(CaeError::contract("fixed masks conflict with provider invariants"));
        }
    }
    Ok(y)
}

