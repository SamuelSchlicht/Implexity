// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use serde_json::{Map, Value};

use implexity_core::{CaeError, CaeResult};

pub(crate) fn refuse<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message.into()))
}

#[derive(Debug)]
pub(crate) struct Section<'a> {
    map: &'a Map<String, Value>,
    path: String,
    out: Map<String, Value>,
}

impl<'a> Section<'a> {
    pub(crate) fn new(value: &'a Value, path: &str, allowed: &[&str]) -> CaeResult<Self> {
        let map =
            value.as_object().ok_or_else(|| CaeError::contract(format!("{path} must be a JSON object")))?;
        let mut unknown: Vec<&str> = map
            .iter()
            .filter(|(k, v)| !v.is_null() && !allowed.contains(&k.as_str()))
            .map(|(k, _)| k.as_str())
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return refuse(format!(
                "{path} has unknown keys {} (allowed: {})",
                unknown.join(", "),
                allowed.join(", ")
            ));
        }
        Ok(Self { map, path: path.to_string(), out: Map::new() })
    }

    pub(crate) fn at(&self, key: &str) -> String {
        format!("{}.{key}", self.path)
    }

    pub(crate) fn raw(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key).filter(|v| !v.is_null())
    }

    pub(crate) fn has(&self, key: &str) -> bool {
        self.raw(key).is_some()
    }

    pub(crate) fn put(&mut self, key: &str, value: Value) {
        self.out.insert(key.to_string(), value);
    }

    pub(crate) fn finish(self) -> Value {
        Value::Object(self.out)
    }

    fn number_of(&self, key: &str, v: &Value) -> CaeResult<f64> {
        match v {
            Value::Number(n) => n
                .as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| CaeError::contract(format!("{} must be a finite number", self.at(key)))),
            _ => refuse(format!("{} must be a finite number", self.at(key))),
        }
    }

    pub(crate) fn number(&mut self, key: &str, ok: impl Fn(f64) -> bool, rule: &str) -> CaeResult<f64> {
        let v = self.raw(key).ok_or_else(|| CaeError::contract(format!("{} is required", self.at(key))))?;
        let x = self.number_of(key, v)?;
        if !ok(x) {
            return refuse(format!("{} must be {rule} (got {x})", self.at(key)));
        }
        self.put(key, Value::from(x));
        Ok(x)
    }

    pub(crate) fn number_or(
        &mut self,
        key: &str,
        default: f64,
        ok: impl Fn(f64) -> bool,
        rule: &str,
    ) -> CaeResult<f64> {
        if self.has(key) {
            return self.number(key, ok, rule);
        }
        self.put(key, Value::from(default));
        Ok(default)
    }

    fn integer_of(&self, key: &str, v: &Value) -> CaeResult<usize> {
        v.as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| CaeError::contract(format!("{} must be a nonnegative integer", self.at(key))))
    }

    pub(crate) fn integer(&mut self, key: &str, range: std::ops::RangeInclusive<usize>) -> CaeResult<usize> {
        let v = self.raw(key).ok_or_else(|| CaeError::contract(format!("{} is required", self.at(key))))?;
        let n = self.integer_of(key, v)?;
        if !range.contains(&n) {
            return refuse(format!(
                "{} must be an integer in {}..={} (got {n})",
                self.at(key),
                range.start(),
                range.end()
            ));
        }
        self.put(key, Value::from(n));
        Ok(n)
    }

    pub(crate) fn integer_or(
        &mut self,
        key: &str,
        default: usize,
        range: std::ops::RangeInclusive<usize>,
    ) -> CaeResult<usize> {
        if self.has(key) {
            return self.integer(key, range);
        }
        self.put(key, Value::from(default));
        Ok(default)
    }

    pub(crate) fn boolean_or(&mut self, key: &str, default: bool) -> CaeResult<bool> {
        let b = match self.raw(key) {
            None => default,
            Some(Value::Bool(b)) => *b,
            Some(_) => return refuse(format!("{} must be a Boolean", self.at(key))),
        };
        self.put(key, Value::Bool(b));
        Ok(b)
    }

    pub(crate) fn choice(&mut self, key: &str, choices: &[&str]) -> CaeResult<String> {
        match self.raw(key) {
            Some(Value::String(s)) if choices.contains(&s.as_str()) => {
                self.put(key, Value::String(s.clone()));
                Ok(s.clone())
            }
            Some(_) => refuse(format!("{} must be one of {}", self.at(key), choices.join(", "))),
            None => refuse(format!("{} is required (one of {})", self.at(key), choices.join(", "))),
        }
    }

    pub(crate) fn choice_or(&mut self, key: &str, default: &str, choices: &[&str]) -> CaeResult<String> {
        if self.has(key) {
            return self.choice(key, choices);
        }
        self.put(key, Value::String(default.to_string()));
        Ok(default.to_string())
    }

    pub(crate) fn text_or(&mut self, key: &str, default: &str, max: usize) -> CaeResult<String> {
        let s = match self.raw(key) {
            None => default.to_string(),
            Some(Value::String(s)) if s.chars().count() <= max => s.clone(),
            Some(_) => return refuse(format!("{} must be a text of at most {max} characters", self.at(key))),
        };
        self.put(key, Value::String(s.clone()));
        Ok(s)
    }

    pub(crate) fn numbers_of(&self, key: &str, v: &Value, len: Option<usize>) -> CaeResult<Vec<f64>> {
        let a = v
            .as_array()
            .ok_or_else(|| CaeError::contract(format!("{} must be a list of numbers", self.at(key))))?;
        if let Some(n) = len
            && a.len() != n
        {
            return refuse(format!("{} must hold {n} numbers", self.at(key)));
        }
        a.iter().map(|x| self.number_of(key, x)).collect()
    }

    pub(crate) fn triple(&mut self, key: &str) -> CaeResult<[f64; 3]> {
        let v = self.raw(key).ok_or_else(|| CaeError::contract(format!("{} is required", self.at(key))))?;
        let x = self.numbers_of(key, v, Some(3))?;
        let t = [x[0], x[1], x[2]];
        self.put(key, Value::from(x));
        Ok(t)
    }

    pub(crate) fn triple_or(&mut self, key: &str, default: [f64; 3]) -> CaeResult<[f64; 3]> {
        if self.has(key) {
            return self.triple(key);
        }
        self.put(key, Value::from(default.to_vec()));
        Ok(default)
    }

    pub(crate) fn flags3_or(&mut self, key: &str, default: [bool; 3]) -> CaeResult<[bool; 3]> {
        let out = match self.raw(key) {
            None => default,
            Some(Value::Array(a)) if a.len() == 3 && a.iter().all(Value::is_boolean) => {
                [a[0].as_bool() == Some(true), a[1].as_bool() == Some(true), a[2].as_bool() == Some(true)]
            }
            Some(_) => return refuse(format!("{} must be three Booleans", self.at(key))),
        };
        self.put(key, Value::from(out.to_vec()));
        Ok(out)
    }

    pub(crate) fn shape(&mut self, key: &str, max_each: usize) -> CaeResult<[usize; 3]> {
        let v = self.raw(key).ok_or_else(|| CaeError::contract(format!("{} is required", self.at(key))))?;
        let a = v
            .as_array()
            .filter(|a| a.len() == 3)
            .ok_or_else(|| CaeError::contract(format!("{} must be three positive integers", self.at(key))))?;
        let mut s = [0usize; 3];
        for (i, x) in a.iter().enumerate() {
            let n = self.integer_of(key, x)?;
            if n == 0 || n > max_each {
                return refuse(format!("{} entries must lie in 1..={max_each}", self.at(key)));
            }
            s[i] = n;
        }
        self.put(key, Value::from(s.to_vec()));
        Ok(s)
    }

    pub(crate) fn child(&self, key: &str, allowed: &[&str]) -> CaeResult<Section<'a>> {
        let v = self.raw(key).ok_or_else(|| CaeError::contract(format!("{} is required", self.at(key))))?;
        Section::new(v, &self.at(key), allowed)
    }

    pub(crate) fn child_or_empty(&self, key: &str, allowed: &[&str]) -> CaeResult<Section<'a>> {
        match self.raw(key) {
            Some(v) => Section::new(v, &self.at(key), allowed),
            None => Ok(Section { map: empty(), path: self.at(key), out: Map::new() }),
        }
    }

    pub(crate) fn list(&self, key: &str, max: usize) -> CaeResult<Vec<(&'a Value, String)>> {
        match self.raw(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(a)) if a.len() <= max => {
                Ok(a.iter().enumerate().map(|(i, v)| (v, format!("{}[{i}]", self.at(key)))).collect())
            }
            Some(_) => refuse(format!("{} must be a list of at most {max} entries", self.at(key))),
        }
    }
}

fn empty() -> &'static Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(Map::new)
}

pub(crate) fn strip_nulls(value: &Value) -> Value {
    match value {
        Value::Object(m) => Value::Object(
            m.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), strip_nulls(v))).collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(strip_nulls).collect()),
        other => other.clone(),
    }
}

pub(crate) fn kind_of<'v>(value: &'v Value, path: &str, choices: &[&str]) -> CaeResult<&'v str> {
    match value.get("kind") {
        Some(Value::String(s)) if choices.contains(&s.as_str()) => Ok(s),
        _ => refuse(format!("{path}.kind must be one of {}", choices.join(", "))),
    }
}

pub(crate) fn box_of(value: &Value, path: &str) -> CaeResult<[[f64; 3]; 2]> {
    let rows = value
        .as_array()
        .filter(|a| a.len() == 2)
        .ok_or_else(|| CaeError::contract(format!("{path} must be [[x0, y0, z0], [x1, y1, z1]]")))?;
    let mut b = [[0.0; 3]; 2];
    for (r, row) in rows.iter().enumerate() {
        let a = row
            .as_array()
            .filter(|a| a.len() == 3)
            .ok_or_else(|| CaeError::contract(format!("{path} must be [[x0, y0, z0], [x1, y1, z1]]")))?;
        for (i, x) in a.iter().enumerate() {
            b[r][i] = x
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| CaeError::contract(format!("{path} must hold finite numbers")))?;
        }
    }
    if (0..3).any(|i| b[0][i] > b[1][i]) {
        return refuse(format!("{path}: the lower corner must not exceed the upper corner"));
    }
    Ok(b)
}

fn row_major(shape: [usize; 3], xfast: usize) -> usize {
    let i = xfast % shape[0];
    let j = (xfast / shape[0]) % shape[1];
    let k = xfast / (shape[0] * shape[1]);
    (i * shape[1] + j) * shape[2] + k
}

fn runs_header<'v>(value: &'v Value, path: &str, shape: [usize; 3]) -> CaeResult<&'v Vec<Value>> {
    let m = value.as_object().ok_or_else(|| {
        CaeError::contract(format!("{path} must be {{\"shape\": [nx, ny, nz], \"runs\": [...]}}"))
    })?;
    if let Some(k) = m.keys().find(|k| !matches!(k.as_str(), "shape" | "runs")) {
        return refuse(format!("{path} has unknown key {k:?} (allowed: shape, runs)"));
    }
    let s = m
        .get("shape")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 3)
        .and_then(|a| {
            let v: Option<Vec<usize>> =
                a.iter().map(|x| x.as_u64().and_then(|n| usize::try_from(n).ok())).collect();
            v
        })
        .ok_or_else(|| CaeError::contract(format!("{path}.shape must hold three integers")))?;
    if s != shape {
        return refuse(format!("{path}.shape {s:?} does not match the grid shape {shape:?}"));
    }
    m.get("runs")
        .and_then(Value::as_array)
        .ok_or_else(|| CaeError::contract(format!("{path}.runs must list [start, length(, value)] runs")))
}

fn run_bounds(row: &[Value], path: &str, total: usize) -> CaeResult<(usize, usize)> {
    let start = row.first().and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok());
    let len = row.get(1).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok());
    match (start, len) {
        (Some(s), Some(l)) if l > 0 && s.checked_add(l).is_some_and(|e| e <= total) => Ok((s, l)),
        _ => refuse(format!("{path}.runs entries must be [start, length] inside the {total} grid entries")),
    }
}


pub(crate) fn mask_of(value: &Value, path: &str, shape: [usize; 3]) -> CaeResult<Vec<bool>> {
    let runs = runs_header(value, path, shape)?;
    let total: usize = shape.iter().product();
    let mut out = vec![false; total];
    for r in runs {
        let row = r
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or_else(|| CaeError::contract(format!("{path}.runs entries must be [start, length]")))?;
        let (s, l) = run_bounds(row, path, total)?;
        for f in s..s + l {
            out[row_major(shape, f)] = true;
        }
    }
    Ok(out)
}


pub(crate) fn values_of(value: &Value, path: &str, shape: [usize; 3], fill: f64) -> CaeResult<Vec<f64>> {
    let runs = runs_header(value, path, shape)?;
    let total: usize = shape.iter().product();
    let mut out = vec![fill; total];
    for r in runs {
        let row = r.as_array().filter(|a| a.len() == 3).ok_or_else(|| {
            CaeError::contract(format!("{path}.runs entries must be [start, length, value]"))
        })?;
        let (s, l) = run_bounds(row, path, total)?;
        let v = row[2]
            .as_f64()
            .filter(|x| x.is_finite())
            .ok_or_else(|| CaeError::contract(format!("{path}.runs values must be finite numbers")))?;
        for f in s..s + l {
            out[row_major(shape, f)] = v;
        }
    }
    Ok(out)
}

pub(crate) fn encode_mask(mask: &[bool], shape: [usize; 3]) -> Value {
    let total: usize = shape.iter().product();
    let flag = |f: usize| -> bool {
        let i = f % shape[0];
        let j = (f / shape[0]) % shape[1];
        let k = f / (shape[0] * shape[1]);
        mask[(i * shape[1] + j) * shape[2] + k]
    };
    let mut runs: Vec<Value> = Vec::new();
    let mut f = 0;
    while f < total {
        if flag(f) {
            let start = f;
            while f < total && flag(f) {
                f += 1;
            }
            runs.push(Value::from(vec![start, f - start]));
        } else {
            f += 1;
        }
    }
    serde_json::json!({"shape": shape.to_vec(), "runs": runs})
}

pub(crate) fn in_box(b: &[[f64; 3]; 2], x: [f64; 3], scale: f64) -> bool {
    let tol = 1e-9 * scale;
    (0..3).all(|i| x[i] >= b[0][i] - tol && x[i] <= b[1][i] + tol)
}
