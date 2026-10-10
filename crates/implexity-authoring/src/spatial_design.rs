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


use serde_json::Value;

use crate::error::{AResult, AuthoringError};
use crate::py::Arr;
use crate::spatial_values::{persist_array_parameter, resolve_array_parameter};

fn err(message: &str) -> AuthoringError {
    AuthoringError::value("SpatialValueError", message)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    Scalar(f64),
    Array(Arr),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpatialDesignSpec {
    pub node_id: String,
    pub parameter: String,
    pub lower: Bound,
    pub upper: Bound,
    pub share_policy: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpatialDesignLayout {
    pub specs: Vec<SpatialDesignSpec>,
    pub shapes: Vec<Vec<usize>>,
    pub offsets: Vec<(usize, usize)>,
    pub size: usize,
}


pub fn build_layout(document: &Value, specs: &[SpatialDesignSpec]) -> AResult<SpatialDesignLayout> {
    let mut shapes = Vec::new();
    let mut offsets = Vec::new();
    let mut start = 0;
    for spec in specs {
        let arr = resolve_array_parameter(document, &spec.node_id, &spec.parameter)?;
        if arr.ndim() == 0 {
            return Err(err("Spatial design coordinate cannot be scalar"));
        }
        let stop = start + arr.size();
        shapes.push(arr.shape.clone());
        offsets.push((start, stop));
        start = stop;
    }
    Ok(SpatialDesignLayout { specs: specs.to_vec(), shapes, offsets, size: start })
}


pub fn flatten_native(document: &Value, layout: &SpatialDesignLayout) -> AResult<Vec<f64>> {
    let mut out = Vec::with_capacity(layout.size);
    for s in &layout.specs {
        out.extend(resolve_array_parameter(document, &s.node_id, &s.parameter)?.data);
    }
    Ok(out)
}

fn broadcast(bound: &Bound, shape: &[usize]) -> AResult<Vec<f64>> {
    let n: usize = shape.iter().product();
    match bound {
        Bound::Scalar(v) => Ok(vec![*v; n]),
        Bound::Array(a) => {

            if a.ndim() > shape.len() {
                return Err(crate::py::value_error("operands could not be broadcast together"));
            }
            let off = shape.len() - a.ndim();
            for (i, d) in a.shape.iter().enumerate() {
                if *d != 1 && *d != shape[off + i] {
                    return Err(crate::py::value_error("operands could not be broadcast together"));
                }
            }
            let mut out = Vec::with_capacity(n);
            let mut idx = vec![0usize; shape.len()];
            for _ in 0..n {
                let mut flat = 0;
                for (i, d) in a.shape.iter().enumerate() {
                    let j = if *d == 1 { 0 } else { idx[off + i] };
                    flat = flat * d + j;
                }
                out.push(a.data[flat]);
                for ax in (0..shape.len()).rev() {
                    idx[ax] += 1;
                    if idx[ax] < shape[ax] {
                        break;
                    }
                    idx[ax] = 0;
                }
            }
            Ok(out)
        }
    }
}

fn bounds(spec: &SpatialDesignSpec, shape: &[usize]) -> AResult<(Vec<f64>, Vec<f64>)> {
    let lo = broadcast(&spec.lower, shape)?;
    let hi = broadcast(&spec.upper, shape)?;
    if !lo.iter().chain(&hi).all(|v| v.is_finite()) || lo.iter().zip(&hi).any(|(l, h)| h <= l) {
        return Err(err("Spatial design bounds must be finite and strictly ordered"));
    }
    Ok((lo, hi))
}


pub fn normalize_native(native: &[f64], layout: &SpatialDesignLayout) -> AResult<Vec<f64>> {
    if native.len() != layout.size {
        return Err(err("Design-vector length mismatch"));
    }
    let mut out = vec![0.0; native.len()];
    for ((spec, shape), (a, b)) in layout.specs.iter().zip(&layout.shapes).zip(&layout.offsets) {
        let (lo, hi) = bounds(spec, shape)?;
        for (i, k) in (*a..*b).enumerate() {
            out[k] = (native[k] - lo[i]) / (hi[i] - lo[i]);
        }
    }
    Ok(out)
}


pub fn denormalize_unit(unit: &[f64], layout: &SpatialDesignLayout, clip: bool) -> AResult<Vec<f64>> {
    if unit.len() != layout.size {
        return Err(err("Design-vector length mismatch"));
    }
    let unit: Vec<f64> = if clip {
        unit.iter().map(|v| crate::field_interaction::np_clip(*v, 0.0, 1.0)).collect()
    } else {
        unit.to_vec()
    };
    let mut out = vec![0.0; unit.len()];
    for ((spec, shape), (a, b)) in layout.specs.iter().zip(&layout.shapes).zip(&layout.offsets) {
        let (lo, hi) = bounds(spec, shape)?;
        for (i, k) in (*a..*b).enumerate() {
            out[k] = lo[i] + unit[k] * (hi[i] - lo[i]);
        }
    }
    Ok(out)
}


pub fn apply_native(document: &Value, native: &[f64], layout: &SpatialDesignLayout) -> AResult<Value> {
    if native.len() != layout.size {
        return Err(err("Design-vector length mismatch"));
    }
    let mut out = document.clone();
    for ((spec, shape), (a, b)) in layout.specs.iter().zip(&layout.shapes).zip(&layout.offsets) {
        let arr = Arr::new(shape.clone(), native[*a..*b].to_vec());
        out = persist_array_parameter(&out, &spec.node_id, &spec.parameter, &arr, &spec.share_policy, None)?
            .document;
    }
    Ok(out)
}


pub fn apply_unit(document: &Value, unit: &[f64], layout: &SpatialDesignLayout) -> AResult<Value> {
    apply_native(document, &denormalize_unit(unit, layout, true)?, layout)
}
