// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::contracts::{CaeProvider, ProviderCapabilities, ProviderProblem, TOPOLOGY_COORDINATE};
use implexity_core::numeric_contract::real_array;
use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value};

use crate::design::{NamedArrays, require_finite};
use crate::numeric::{array_to_value, bool_array_to_value, float_value, shape_repr, str_list_repr};
use crate::provider_ops::{
    DesignOp, DesignSensitivity, design_operations, legacy_evaluate, legacy_sensitivity,
};

pub const REGULARIZATION_KEYS: [&str; 6] =
    ["filter_radius_m", "projection_beta", "projection_eta", "minimum_fraction", "simp_penalty", "spacing_m"];

#[derive(Debug, Clone, PartialEq)]
pub enum Spacing {
    Scalar(f64),
    PerAxis(Vec<f64>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Regularization {
    pub filter_radius_m: Option<f64>,
    pub projection_beta: Option<f64>,
    pub projection_eta: Option<f64>,
    pub minimum_fraction: Option<f64>,
    pub simp_penalty: Option<f64>,
    pub spacing_m: Option<Spacing>,
}

impl Regularization {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        for (k, v) in [
            ("filter_radius_m", self.filter_radius_m),
            ("projection_beta", self.projection_beta),
            ("projection_eta", self.projection_eta),
            ("minimum_fraction", self.minimum_fraction),
            ("simp_penalty", self.simp_penalty),
        ] {
            if let Some(v) = v {
                m.insert(k.into(), float_value(v));
            }
        }
        match &self.spacing_m {
            None => {}
            Some(Spacing::Scalar(s)) => {
                m.insert("spacing_m".into(), float_value(*s));
            }
            Some(Spacing::PerAxis(v)) => {
                m.insert("spacing_m".into(), Value::Array(v.iter().copied().map(float_value).collect()));
            }
        }

        Value::Object(m)
    }
}

fn scalar_of(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}


pub fn validate_regularization(
    name: &str,
    raw: Option<&Value>,
    ndim: usize,
) -> CaeResult<Option<Regularization>> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(map) = raw.as_object() else {
        return Err(CaeError::contract(format!("{name}: regularization must be a mapping or None")));
    };
    let mut unknown: Vec<&String> =
        map.keys().filter(|k| !REGULARIZATION_KEYS.contains(&k.as_str())).collect();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(CaeError::contract(format!(
            "{name}: unknown regularization keys {}; accepted keys are {}",
            str_list_repr(&unknown),
            str_list_repr(&REGULARIZATION_KEYS)
        )));
    }
    let mut out = Regularization::default();
    for key in REGULARIZATION_KEYS {
        let Some(value) = map.get(key) else { continue };
        if key == "spacing_m" {
            let bad = || {
                CaeError::contract(format!("{name}: regularization.spacing_m must be finite and positive"))
            };
            match value {
                Value::Number(_) => {
                    let s = scalar_of(value).ok_or_else(bad)?;
                    if !s.is_finite() || s <= 0.0 {
                        return Err(bad());
                    }
                    out.spacing_m = Some(Spacing::Scalar(s));
                }
                Value::Array(items) => {
                    let vals: Option<Vec<f64>> = items.iter().map(scalar_of).collect();
                    let Some(vals) = vals else { return Err(bad()) };
                    if vals.iter().any(|v| !v.is_finite() || *v <= 0.0) {
                        return Err(bad());
                    }
                    if vals.len() != ndim {
                        return Err(CaeError::contract(format!(
                            "{name}: regularization.spacing_m must be a scalar or one entry per axis ({ndim})"
                        )));
                    }
                    out.spacing_m = Some(Spacing::PerAxis(vals));
                }
                _ => return Err(bad()),
            }
            continue;
        }
        let v = match value {
            Value::Number(_) => scalar_of(value).filter(|v| v.is_finite()),
            _ => None,
        };
        let Some(v) = v else {
            return Err(CaeError::contract(format!(
                "{name}: regularization.{key} must be a finite real scalar"
            )));
        };
        match key {
            "filter_radius_m" if v < 0.0 => {
                return Err(CaeError::contract(format!(
                    "{name}: regularization.filter_radius_m must be non-negative"
                )));
            }
            "projection_beta" if !(0.0..=32.0).contains(&v) => {
                return Err(CaeError::contract(format!(
                    "{name}: regularization.projection_beta must lie in [0, 32]"
                )));
            }
            "projection_eta" if !(0.0 < v && v < 1.0) => {
                return Err(CaeError::contract(format!(
                    "{name}: regularization.projection_eta must lie in (0, 1)"
                )));
            }
            "minimum_fraction" if !(0.0..1.0).contains(&v) => {
                return Err(CaeError::contract(format!(
                    "{name}: regularization.minimum_fraction must lie in [0, 1)"
                )));
            }
            "simp_penalty" if v < 1.0 => {
                return Err(CaeError::contract(format!(
                    "{name}: regularization.simp_penalty must be at least one"
                )));
            }
            _ => {}
        }
        match key {
            "filter_radius_m" => out.filter_radius_m = Some(v),
            "projection_beta" => out.projection_beta = Some(v),
            "projection_eta" => out.projection_eta = Some(v),
            "minimum_fraction" => out.minimum_fraction = Some(v),
            _ => out.simp_penalty = Some(v),
        }
    }
    Ok(Some(out))
}

#[derive(Debug, Clone)]
pub struct DesignCoordinate {
    pub name: String,
    pub value: ArrayD<f64>,
    pub lower: ArrayD<f64>,
    pub upper: ArrayD<f64>,
    pub designable: ArrayD<bool>,
    pub regularization: Option<Regularization>,
}

impl PartialEq for DesignCoordinate {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.value == other.value
            && self.lower == other.lower
            && self.upper == other.upper
            && self.designable == other.designable
    }
}

impl DesignCoordinate {

    pub fn new(
        name: &str,
        value: ArrayD<f64>,
        lower: ArrayD<f64>,
        upper: ArrayD<f64>,
        designable: ArrayD<bool>,
        regularization: Option<&Value>,
    ) -> CaeResult<Self> {
        if name.is_empty() {
            return Err(CaeError::contract("design coordinate name must be nonempty text"));
        }
        require_finite(&value, &format!("{name}: design value"))?;
        require_finite(&lower, &format!("{name}: lower bound"))?;
        require_finite(&upper, &format!("{name}: upper bound"))?;
        let reg = validate_regularization(name, regularization, value.ndim())?;
        Self::checked(name, value, lower, upper, designable, reg)
    }

    fn checked(
        name: &str,
        value: ArrayD<f64>,
        lower: ArrayD<f64>,
        upper: ArrayD<f64>,
        designable: ArrayD<bool>,
        regularization: Option<Regularization>,
    ) -> CaeResult<Self> {
        if value.ndim() == 0 || value.is_empty() {
            return Err(CaeError::contract(format!("{name}: design value must be finite and nonempty")));
        }
        if lower.shape() != value.shape() || upper.shape() != value.shape() {
            return Err(CaeError::contract(format!(
                "{name}: bounds must match design shape {}",
                shape_repr(value.shape())
            )));
        }
        if lower.iter().zip(upper.iter()).any(|(l, u)| l >= u) {
            return Err(CaeError::contract(format!(
                "{name}: design bounds must be finite and strictly ordered"
            )));
        }
        if value.iter().zip(lower.iter()).any(|(v, l)| *v < l - 1e-12)
            || value.iter().zip(upper.iter()).any(|(v, u)| *v > u + 1e-12)
        {
            return Err(CaeError::contract(format!("{name}: design value lies outside declared bounds")));
        }
        if designable.shape() != value.shape() {
            return Err(CaeError::contract(format!(
                "{name}: designable mask must be boolean shape {}",
                shape_repr(value.shape())
            )));
        }
        Ok(Self { name: name.to_string(), value, lower, upper, designable, regularization })
    }

    #[must_use]
    pub fn span(&self) -> ArrayD<f64> {
        &self.upper - &self.lower
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("value".into(), array_to_value(&self.value));
        m.insert("lower".into(), array_to_value(&self.lower));
        m.insert("upper".into(), array_to_value(&self.upper));
        m.insert("designable".into(), bool_array_to_value(&self.designable));
        if let Some(r) = &self.regularization {
            m.insert("regularization".into(), r.to_value());
        }
        Value::Object(m)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesignState {
    pub coordinates: Vec<DesignCoordinate>,
}

impl DesignState {

    pub fn new(coordinates: Vec<DesignCoordinate>) -> CaeResult<Self> {
        if coordinates.is_empty() {
            return Err(CaeError::contract("design state requires at least one coordinate"));
        }
        let mut names: Vec<&str> = coordinates.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        if names.len() != coordinates.len() {
            return Err(CaeError::contract("design state coordinate names must be unique"));
        }
        Ok(Self { coordinates })
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.coordinates.iter().map(|c| c.name.clone()).collect()
    }

    #[must_use]
    pub fn values(&self) -> NamedArrays {
        self.coordinates.iter().map(|c| (c.name.clone(), c.value.clone())).collect()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&DesignCoordinate> {
        self.coordinates.iter().find(|c| c.name == name)
    }


    pub fn with_values(&self, values: &NamedArrays) -> CaeResult<Self> {
        let mut rows = Vec::with_capacity(self.coordinates.len());
        for c in &self.coordinates {
            let Some(v) = values.get(&c.name) else {
                return Err(CaeError::contract(format!(
                    "design update omitted coordinate {}",
                    repr_str(&c.name)
                )));
            };
            require_finite(v, &format!("design update for {}", repr_str(&c.name)))?;
            if v.shape() != c.value.shape() {
                return Err(CaeError::contract(format!(
                    "design update for {} has invalid shape or non-finite values",
                    repr_str(&c.name)
                )));
            }
            rows.push(DesignCoordinate::checked(
                &c.name,
                v.clone(),
                c.lower.clone(),
                c.upper.clone(),
                c.designable.clone(),
                c.regularization.clone(),
            )?);
        }
        let mut extra: Vec<String> =
            values.names().into_iter().filter(|n| !self.coordinates.iter().any(|c| &c.name == n)).collect();
        extra.sort();
        if !extra.is_empty() {
            return Err(CaeError::contract(format!(
                "design update contains unknown coordinates {}",
                str_list_repr(&extra)
            )));
        }
        Self::new(rows)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        Value::Object(self.coordinates.iter().map(|c| (c.name.clone(), c.to_value())).collect())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BoundInput {
    Scalar(f64),
    Json(Value),
}


pub fn full_like(value: &ArrayD<f64>, raw: &Value, label: &str, name: &str) -> CaeResult<ArrayD<f64>> {
    let a = real_array(raw, &format!("{name}: {label}"))?;
    let a = if a.ndim() == 0 {
        ArrayD::from_elem(value.raw_dim(), a.iter().next().copied().unwrap_or(0.0))
    } else {
        a
    };
    if a.shape() != value.shape() {
        return Err(CaeError::contract(format!(
            "{name}: {label} must be finite scalar or shape {}",
            shape_repr(value.shape())
        )));
    }
    Ok(a)
}

fn bound_value(b: &BoundInput) -> Value {
    match b {
        BoundInput::Scalar(s) => float_value(*s),
        BoundInput::Json(v) => v.clone(),
    }
}

fn designable_mask(raw: &Value, shape: &[usize], name: &str) -> CaeResult<ArrayD<bool>> {
    let bad = || {
        CaeError::contract(format!(
            "{name}: designable mask must be boolean scalar or shape {}",
            shape_repr(shape)
        ))
    };
    match raw {
        Value::Bool(b) => Ok(ArrayD::from_elem(IxDyn(shape), *b)),
        Value::Array(_) => {
            fn walk(v: &Value, out: &mut Vec<bool>, dims: &mut Vec<usize>, depth: usize) -> bool {
                match v {
                    Value::Bool(b) => {
                        if depth != dims.len() {
                            return false;
                        }
                        out.push(*b);
                        true
                    }
                    Value::Array(items) => {
                        if depth == dims.len() {
                            if !out.is_empty() {
                                return false;
                            }
                            dims.push(items.len());
                        } else if depth > dims.len() || dims[depth] != items.len() {
                            return false;
                        }
                        items.iter().all(|i| walk(i, out, dims, depth + 1))
                    }
                    _ => false,
                }
            }
            let mut data = Vec::new();
            let mut dims = Vec::new();
            if !walk(raw, &mut data, &mut dims, 0) || dims.as_slice() != shape {
                return Err(bad());
            }
            ArrayD::from_shape_vec(IxDyn(shape), data).map_err(|_| bad())
        }
        Value::Number(_) | Value::Null | Value::String(_) | Value::Object(_) => {
            Err(CaeError::contract(format!("{name}: scalar designable flag must be boolean")))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormaliseOptions {
    pub coordinate_names: Option<Vec<String>>,
    pub coordinate_lower: Option<BoundInput>,
    pub coordinate_upper: Option<BoundInput>,
    pub topology_lower: f64,
    pub topology_upper: f64,
}

impl Default for NormaliseOptions {
    fn default() -> Self {
        Self {
            coordinate_names: None,
            coordinate_lower: None,
            coordinate_upper: None,
            topology_lower: 0.0,
            topology_upper: 1.0,
        }
    }
}

impl NormaliseOptions {
    #[must_use]
    pub fn named(names: &[String]) -> Self {
        Self { coordinate_names: Some(names.to_vec()), ..Self::default() }
    }
}


#[allow(clippy::too_many_lines)]
pub fn normalise_design(raw: &Value, options: &NormaliseOptions) -> CaeResult<DesignState> {
    let compatibility = options.coordinate_names.is_none();
    let names: Vec<String> = match &options.coordinate_names {
        Some(n) if !n.is_empty() => n.clone(),
        _ => vec![TOPOLOGY_COORDINATE.to_string()],
    };
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    if names.iter().any(String::is_empty) || unique.len() != names.len() {
        return Err(CaeError::contract("declared design coordinates must be unique nonempty names"));
    }
    let wrapped;
    let map: &Map<String, Value> = match raw {
        Value::Object(m) => {
            if m.keys().any(String::is_empty) {
                return Err(CaeError::contract("design coordinate ids must be nonempty text"));
            }
            m
        }
        other => {
            if names.len() != 1 {
                return Err(CaeError::contract(
                    "multi-coordinate optimisation requires a design mapping, not a bare topology array",
                ));
            }
            if !compatibility {
                return Err(CaeError::contract(
                    "strict named-design coordinates require an explicit mapping, not a bare array",
                ));
            }
            let mut item = Map::new();
            item.insert("value".into(), other.clone());
            item.insert("lower".into(), float_value(options.topology_lower));
            item.insert("upper".into(), float_value(options.topology_upper));
            let mut m = Map::new();
            m.insert(names[0].clone(), Value::Object(item));
            wrapped = m;
            &wrapped
        }
    };
    let missing: Vec<&String> = names.iter().filter(|n| !map.contains_key(*n)).collect();
    let extra: Vec<&String> = map.keys().filter(|n| !names.contains(n)).collect();
    if !missing.is_empty() || !extra.is_empty() {
        let mut problems = Vec::new();
        if !missing.is_empty() {
            problems.push(format!("design is missing declared coordinates {}", str_list_repr(&missing)));
        }
        if !extra.is_empty() {
            problems.push(format!("design supplies undeclared coordinates {}", str_list_repr(&extra)));
        }
        return Err(CaeError::contract(problems.join("; ")));
    }
    let mut rows = Vec::with_capacity(names.len());
    for name in &names {
        let item = &map[name];
        let default_bound = |explicit: &Option<BoundInput>, topology: f64| -> Option<Value> {
            match explicit {
                Some(b) => Some(bound_value(b)),
                None if compatibility && name == TOPOLOGY_COORDINATE => Some(float_value(topology)),
                None => None,
            }
        };
        let (value, lo_raw, hi_raw, designable_raw, regularization_raw) = if let Value::Object(item) = item {
            let raw_value = match item.get("value") {
                Some(v) => v,
                None => match item.get("values") {
                    Some(v) => v,
                    None => {
                        return Err(CaeError::contract(format!(
                            "{name}: design coordinate requires value/values"
                        )));
                    }
                },
            };
            let value = real_array(raw_value, &format!("{name}: design coordinate"))?;
            let lo = match item.get("lower") {
                Some(v) => Some(v.clone()).filter(|v| !v.is_null()),
                None => default_bound(&options.coordinate_lower, options.topology_lower),
            };
            let hi = match item.get("upper") {
                Some(v) => Some(v.clone()).filter(|v| !v.is_null()),
                None => default_bound(&options.coordinate_upper, options.topology_upper),
            };
            let designable = item.get("designable").cloned().unwrap_or(Value::Bool(true));
            (value, lo, hi, designable, item.get("regularization").cloned())
        } else {
            let value = real_array(item, &format!("{name}: design coordinate"))?;
            (
                value,
                default_bound(&options.coordinate_lower, options.topology_lower),
                default_bound(&options.coordinate_upper, options.topology_upper),
                Value::Bool(true),
                None,
            )
        };
        if value.ndim() == 0 {
            return Err(CaeError::contract(format!(
                "{name}: design coordinate must be a finite non-scalar array"
            )));
        }
        let (Some(lo_raw), Some(hi_raw)) = (lo_raw, hi_raw) else {
            return Err(CaeError::contract(format!(
                "{name}: design coordinates require explicit lower and upper bounds"
            )));
        };
        let lo = full_like(&value, &lo_raw, "lower", name)?;
        let hi = full_like(&value, &hi_raw, "upper", name)?;
        if lo.iter().zip(hi.iter()).any(|(l, h)| l >= h) {
            return Err(CaeError::contract(format!(
                "{name}: every lower bound must be below its upper bound"
            )));
        }
        if value.iter().zip(lo.iter()).any(|(v, l)| *v < l - 1e-12)
            || value.iter().zip(hi.iter()).any(|(v, h)| *v > h + 1e-12)
        {
            return Err(CaeError::contract(format!("{name}: initial value lies outside declared bounds")));
        }
        let mask = designable_mask(&designable_raw, value.shape(), name)?;
        rows.push(DesignCoordinate::new(name, value, lo, hi, mask, regularization_raw.as_ref())?);
    }
    DesignState::new(rows)
}


pub fn design_state_from_values(values: &NamedArrays, lower: f64, upper: f64) -> CaeResult<DesignState> {
    let mut rows = Vec::new();
    for (name, value) in values.iter() {
        rows.push(DesignCoordinate::new(
            name,
            value.clone(),
            ArrayD::from_elem(value.raw_dim(), lower),
            ArrayD::from_elem(value.raw_dim(), upper),
            ArrayD::from_elem(value.raw_dim(), true),
            None,
        )?);
    }
    DesignState::new(rows)
}

struct Cone {
    kernel: Vec<(Vec<isize>, f64)>,
}

fn cone(shape: &[usize], radius_cells: &[f64]) -> Cone {
    let reach: Vec<usize> = radius_cells
        .iter()
        .zip(shape)
        .map(|(r, n)| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let c = r.ceil().max(0.0) as usize;
            c.min(n.saturating_sub(1))
        })
        .collect();
    let mut kernel = Vec::new();
    let mut offset: Vec<isize> = reach.iter().map(|k| -isize::try_from(*k).unwrap_or(0)).collect();
    loop {
        let mut d2 = 0.0_f64;
        for (o, r) in offset.iter().zip(radius_cells) {
            #[allow(clippy::cast_precision_loss)]
            let a = *o as f64 / r.max(1e-300);
            d2 += a * a;
        }
        let w = f64::max(0.0, 1.0 - d2.sqrt());
        if w != 0.0 {
            kernel.push((offset.clone(), w));
        }
        let mut axis = offset.len();
        loop {
            if axis == 0 {
                return Cone { kernel };
            }
            axis -= 1;
            let k = isize::try_from(reach[axis]).unwrap_or(0);
            if offset[axis] < k {
                offset[axis] += 1;
                for (j, later) in offset.iter_mut().enumerate().skip(axis + 1) {
                    *later = -isize::try_from(reach[j]).unwrap_or(0);
                }
                break;
            }
        }
    }
}

fn correlate(x: &ArrayD<f64>, cone: &Cone) -> ArrayD<f64> {
    let shape = x.shape().to_vec();
    let mut out = ArrayD::zeros(IxDyn(&shape));
    let strides: Vec<usize> = {
        let mut s = vec![1usize; shape.len()];
        for i in (0..shape.len().saturating_sub(1)).rev() {
            s[i] = s[i + 1] * shape[i + 1];
        }
        s
    };
    let data: Vec<f64> = x.iter().copied().collect();
    let mut idx = vec![0usize; shape.len()];
    for (flat, slot) in out.iter_mut().enumerate() {
        let mut rem = flat;
        for (i, s) in strides.iter().enumerate() {
            idx[i] = rem / s;
            rem %= s;
        }
        let mut acc = 0.0;
        'k: for (off, w) in &cone.kernel {
            let mut pos = 0usize;
            for i in 0..shape.len() {
                let p = isize::try_from(idx[i]).unwrap_or(0) + off[i];
                if p < 0 || p >= isize::try_from(shape[i]).unwrap_or(0) {
                    continue 'k;
                }
                pos += usize::try_from(p).unwrap_or(0) * strides[i];
            }
            acc += w * data[pos];
        }
        *slot = acc;
    }
    out
}

fn filter_setup(
    coord: &DesignCoordinate,
    x: &ArrayD<f64>,
    spacing: Option<&Spacing>,
) -> CaeResult<Option<Cone>> {
    let Some(schedule) = &coord.regularization else { return Ok(None) };
    let radius = schedule.filter_radius_m.unwrap_or(0.0);
    if radius <= 0.0 {
        return Ok(None);
    }
    if !(1..=3).contains(&x.ndim()) {
        return Err(CaeError::contract(format!(
            "{}: the cone filter supports 1-, 2- or 3-D fields",
            coord.name
        )));
    }
    let h: Vec<f64> = match spacing.or(schedule.spacing_m.as_ref()) {
        None => vec![1.0; x.ndim()],
        Some(Spacing::Scalar(s)) => vec![*s; x.ndim()],
        Some(Spacing::PerAxis(v)) => v.clone(),
    };
    if h.len() != x.ndim() || h.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(CaeError::contract(format!(
            "{}: spacing_m must be positive, scalar or per-axis",
            coord.name
        )));
    }
    let radius_cells: Vec<f64> = h.iter().map(|hi| radius / hi).collect();
    Ok(Some(cone(x.shape(), &radius_cells)))
}

struct Chain {
    after_filter: ArrayD<f64>,
    after_projection: ArrayD<f64>,
    denominator: Option<ArrayD<f64>>,
    cone: Option<Cone>,
}

fn forward(
    coord: &DesignCoordinate,
    raw: &ArrayD<f64>,
    spacing: Option<&Spacing>,
) -> CaeResult<Option<(Chain, ArrayD<f64>)>> {
    let Some(schedule) = &coord.regularization else { return Ok(None) };
    if raw.shape() != coord.value.shape() {
        return Err(CaeError::contract(format!(
            "{}: value shape {} does not match {}",
            coord.name,
            shape_repr(raw.shape()),
            shape_repr(coord.value.shape())
        )));
    }
    let cone = filter_setup(coord, raw, spacing)?;
    let (after_filter, denominator) = match &cone {
        None => (raw.clone(), None),
        Some(c) => {
            let num = correlate(raw, c);
            let den = correlate(&ArrayD::from_elem(raw.raw_dim(), 1.0), c);
            let filtered = &num / &den;
            let mut x = raw.clone();
            ndarray::Zip::from(&mut x).and(&filtered).and(&coord.designable).for_each(|x, f, d| {
                if *d {
                    *x = *f;
                }
            });
            (x, Some(den))
        }
    };
    let beta = schedule.projection_beta.unwrap_or(0.0);
    let after_projection = if beta > 0.0 {
        let eta = schedule.projection_eta.unwrap_or(0.5);
        let a = (beta * eta).tanh();
        let den = a + (beta * (1.0 - eta)).tanh();
        after_filter.mapv(|x| (a + (beta * (x - eta)).tanh()) / den)
    } else {
        after_filter.clone()
    };
    let penalty = schedule.simp_penalty.unwrap_or(1.0);
    #[allow(clippy::float_cmp)]                                         
    let after_simp = if penalty == 1.0 {
        after_projection.clone()
    } else {
        after_projection.mapv(|x| x.clamp(0.0, 1.0).powf(penalty))
    };
    let floor = schedule.minimum_fraction.unwrap_or(0.0);
    let out = if floor > 0.0 { after_simp.mapv(|x| floor + (1.0 - floor) * x) } else { after_simp };
    Ok(Some((Chain { after_filter, after_projection, denominator, cone }, out)))
}


pub fn apply_regularization(
    coord: &DesignCoordinate,
    value: Option<&ArrayD<f64>>,
    spacing: Option<&Spacing>,
) -> CaeResult<ArrayD<f64>> {
    let raw = value.unwrap_or(&coord.value);
    Ok(match forward(coord, raw, spacing)? {
        None => raw.clone(),
        Some((_, out)) => out,
    })
}


pub fn regularization_vjp(
    coord: &DesignCoordinate,
    value: Option<&ArrayD<f64>>,
    cotangent: &ArrayD<f64>,
    spacing: Option<&Spacing>,
) -> CaeResult<ArrayD<f64>> {
    let raw = value.unwrap_or(&coord.value);
    let Some((chain, _)) = forward(coord, raw, spacing)? else {
        return Ok(cotangent.clone());
    };
    let Some(schedule) = &coord.regularization else { return Ok(cotangent.clone()) };
    if cotangent.shape() != raw.shape() {
        return Err(CaeError::contract(format!("{}: cotangent shape does not match the design", coord.name)));
    }
    let floor = schedule.minimum_fraction.unwrap_or(0.0);
    let mut g = if floor > 0.0 { cotangent.mapv(|c| (1.0 - floor) * c) } else { cotangent.clone() };
    let penalty = schedule.simp_penalty.unwrap_or(1.0);
    #[allow(clippy::float_cmp)]
    if penalty != 1.0 {
        ndarray::Zip::from(&mut g).and(&chain.after_projection).for_each(|g, x| {
            let c = x.clamp(0.0, 1.0);
            let dclip = if *x > 0.0 && *x < 1.0 {
                1.0
            } else if *x == 0.0 || *x == 1.0 {
                0.5
            } else {
                0.0
            };
            *g *= penalty * c.powf(penalty - 1.0) * dclip;
        });
    }
    let beta = schedule.projection_beta.unwrap_or(0.0);
    if beta > 0.0 {
        let eta = schedule.projection_eta.unwrap_or(0.5);
        let den = (beta * eta).tanh() + (beta * (1.0 - eta)).tanh();
        ndarray::Zip::from(&mut g).and(&chain.after_filter).for_each(|g, x| {
            let t = (beta * (x - eta)).tanh();
            *g *= beta * (1.0 - t * t) / den;
        });
    }
    if let (Some(c), Some(den)) = (&chain.cone, &chain.denominator) {
        let mut scaled = ArrayD::zeros(g.raw_dim());
        let mut passthrough = ArrayD::zeros(g.raw_dim());
        ndarray::Zip::from(&mut scaled)
            .and(&mut passthrough)
            .and(&g)
            .and(den)
            .and(&coord.designable)
            .for_each(|s, p, g, d, m| {
                if *m {
                    *s = g / d;
                } else {
                    *p = *g;
                }
            });
        g = correlate(&scaled, c) + passthrough;
    }
    Ok(g)
}

fn legacy_coordinate(provider: &dyn CaeProvider, state: &DesignState, operation: &str) -> CaeResult<String> {
    let name = provider.name();
    let caps = provider.capabilities()?;
    let ProviderCapabilities::Legacy(legacy) = caps else {
        return Err(CaeError::contract(format!(
            "provider {} has no {operation}_design() for multi-coordinate design",
            repr_str(name)
        )));
    };
    let coordinate = legacy.topology_coordinate.clone();
    if state.names() != [coordinate.clone()] {
        return Err(CaeError::contract(format!(
            "legacy provider {} requires exactly {}",
            repr_str(name),
            repr_str(&coordinate)
        )));
    }
    Ok(coordinate)
}


pub fn provider_evaluate(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    state: &DesignState,
) -> CaeResult<implexity_core::contracts::Evaluation> {
    let values = state.values();
    if let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::EvaluateDesign)) {
        return ops.evaluate_design(problem, &values, 0);
    }
    let coordinate = legacy_coordinate(provider, state, "evaluate")?;
    legacy_array_operation(provider, DesignOp::Evaluate, &coordinate)?;
    let topology = values.get(&coordinate).cloned().unwrap_or_default();
    legacy_evaluate(provider, problem, &topology, 0)
}

fn legacy_array_operation(provider: &dyn CaeProvider, op: DesignOp, coordinate: &str) -> CaeResult<()> {
    if crate::provider_ops::provides(provider, op) {
        return Ok(());
    }
    let name = repr_str(provider.name());
    let operation = op.name();
    Err(CaeError::contract(format!(
        "provider {name} has no {operation} hook -- cannot dispatch legacy single-array {operation}() on {name} with coordinate {}; implement {operation}_design() or the legacy single-array {operation}() operation",
        repr_str(coordinate)
    )))
}


pub fn provider_sensitivity(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    state: &DesignState,
    response: &str,
) -> CaeResult<DesignSensitivity> {
    let values = state.values();
    if let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::SensitivityDesign)) {
        let out = ops.sensitivity_design(problem, &values, response, 0)?;
        implexity_core::numeric_contract::real_scalar_f64(out.value, "sensitivity_design() response")?;
        let mut gradients = NamedArrays::new();
        for (k, v) in out.gradients.iter() {
            require_finite(v, &format!("gradient for {}", repr_str(k)))?;
            gradients.insert(k, v.clone());
        }
        return Ok(DesignSensitivity { value: out.value, gradients, diagnostics: out.diagnostics });
    }
    let coordinate = legacy_coordinate(provider, state, "sensitivity")?;
    legacy_array_operation(provider, DesignOp::Sensitivity, &coordinate)?;
    let topology = values.get(&coordinate).cloned().unwrap_or_default();
    let s = legacy_sensitivity(provider, problem, &topology, response, 0)?;
    implexity_core::numeric_contract::real_scalar_f64(s.value, "legacy sensitivity() response")?;
    require_finite(&s.gradient, "legacy sensitivity gradient")?;
    Ok(DesignSensitivity {
        value: s.value,
        gradients: NamedArrays::single(&coordinate, s.gradient),
        diagnostics: s.diagnostics,
    })
}

