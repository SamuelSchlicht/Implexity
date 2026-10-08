// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
