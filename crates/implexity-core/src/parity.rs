// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::json::parse_strict_bytes;

#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    pub label: String,
    pub index: usize,
    pub actual: f64,
    pub expected: f64,
    pub abs_error: f64,
    pub allowed: f64,
    pub failures: usize,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} entries outside tolerance; worst at index {}: actual {:e}, expected {:e}, |error| {:e} > allowed {:e}",
            self.label, self.failures, self.index, self.actual, self.expected, self.abs_error, self.allowed
        )
    }
}


pub fn compare_close(
    label: &str,
    actual: &[f64],
    expected: &[f64],
    rtol: f64,
    atol: f64,
) -> Result<(), Mismatch> {
    if actual.len() != expected.len() {
        #[allow(clippy::cast_precision_loss)]
        return Err(Mismatch {
            label: format!("{label} (length {} vs {})", actual.len(), expected.len()),
            index: usize::MAX,
            actual: actual.len() as f64,
            expected: expected.len() as f64,
            abs_error: f64::INFINITY,
            allowed: 0.0,
            failures: 1,
        });
    }
    let mut worst: Option<(f64, Mismatch)> = None;
    let mut failures = 0;
    for (i, (&a, &e)) in actual.iter().zip(expected).enumerate() {
        let ok = if e.is_nan() || a.is_nan() {
            e.is_nan() && a.is_nan()
        } else if e.is_infinite() || a.is_infinite() {
            a.to_bits() == e.to_bits()
        } else {
            (a - e).abs() <= atol + rtol * e.abs()
        };
        if !ok {
            failures += 1;
            let allowed = atol + rtol * e.abs();
            let err = (a - e).abs();
            let excess = if err.is_nan() { f64::INFINITY } else { err - allowed };
            if worst.as_ref().is_none_or(|(w, _)| excess > *w) {
                worst = Some((
                    excess,
                    Mismatch {
                        label: label.to_string(),
                        index: i,
                        actual: a,
                        expected: e,
                        abs_error: err,
                        allowed,
                        failures: 0,
                    },
                ));
            }
        }
    }
    match worst {
        None => Ok(()),
        Some((_, mut m)) => {
            m.failures = failures;
            Err(m)
        }
    }
}


#[allow(clippy::panic)]
pub fn assert_close(label: &str, actual: &[f64], expected: &[f64], rtol: f64, atol: f64) {
    if let Err(m) = compare_close(label, actual, expected, rtol, atol) {
        panic!("{m}");
    }
}


pub fn compare_max_norm(label: &str, actual: &[f64], expected: &[f64], rtol: f64) -> Result<(), Mismatch> {
    let scale = expected.iter().fold(0.0_f64, |m, e| m.max(e.abs())).max(f64::MIN_POSITIVE);
    compare_close(label, actual, expected, 0.0, rtol * scale)
}


#[allow(clippy::panic)]
pub fn assert_max_norm(label: &str, actual: &[f64], expected: &[f64], rtol: f64) {
    if let Err(m) = compare_max_norm(label, actual, expected, rtol) {
        panic!("{m}");
    }
}

#[derive(Debug, Clone, Default)]
pub struct JsonCompare {
    pub rtol: f64,
    pub atol: f64,
    pub ignore_keys: Vec<String>,
    pub int_float_equal: bool,
}


pub fn json_equal(actual: &Value, expected: &Value, options: &JsonCompare) -> Result<(), String> {
    json_diff(actual, expected, options, "$")
}

fn json_diff(a: &Value, e: &Value, o: &JsonCompare, path: &str) -> Result<(), String> {
    match (a, e) {
        (Value::Object(ma), Value::Object(me)) => {
            for k in me.keys() {
                if o.ignore_keys.iter().any(|x| x == k) {
                    continue;
                }
                match ma.get(k) {
                    None => return Err(format!("{path}.{k}: missing in actual")),
                    Some(v) => json_diff(v, &me[k], o, &format!("{path}.{k}"))?,
                }
            }
            for k in ma.keys() {
                if !me.contains_key(k) && !o.ignore_keys.iter().any(|x| x == k) {
                    return Err(format!("{path}.{k}: unexpected key in actual"));
                }
            }
            Ok(())
        }
        (Value::Array(va), Value::Array(ve)) => {
            if va.len() != ve.len() {
                return Err(format!("{path}: length {} vs expected {}", va.len(), ve.len()));
            }
            for (i, (x, y)) in va.iter().zip(ve).enumerate() {
                json_diff(x, y, o, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        (Value::Number(na), Value::Number(ne)) => {
            let same_kind = na.is_f64() == ne.is_f64();
            if !same_kind && !o.int_float_equal {
                return Err(format!("{path}: number kind differs ({na} vs expected {ne})"));
            }
            if na.is_f64() || ne.is_f64() {
                let (x, y) = (na.as_f64().unwrap_or(f64::NAN), ne.as_f64().unwrap_or(f64::NAN));
                if (x - y).abs() <= o.atol + o.rtol * y.abs() {
                    Ok(())
                } else {
                    Err(format!("{path}: {x:e} vs expected {y:e}"))
                }
            } else if na == ne {
                Ok(())
            } else {
                Err(format!("{path}: {na} vs expected {ne}"))
            }
        }
        _ => {
            if a == e {
                Ok(())
            } else {
                Err(format!("{path}: {a} vs expected {e}"))
            }
        }
    }
}

#[must_use]
pub fn workspace_root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.parent().and_then(Path::parent).map_or_else(|| here.to_path_buf(), Path::to_path_buf)
}

#[must_use]
pub fn fixtures_dir() -> PathBuf {
    std::env::var_os("IMPLEXITY_FIXTURES").map_or_else(|| workspace_root().join("fixtures"), PathBuf::from)
}

#[must_use]
pub fn fixture_path(relative: &str) -> PathBuf {
    fixtures_dir().join(relative)
}


pub fn load_fixture_json(relative: &str) -> Result<Value, String> {
    let path = fixture_path(relative);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_strict_bytes(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}


pub fn json_f64s(value: &Value) -> Result<Vec<f64>, String> {
    let mut out = Vec::new();
    flatten(value, &mut out)?;
    Ok(out)
}

fn flatten(value: &Value, out: &mut Vec<f64>) -> Result<(), String> {
    match value {
        Value::Number(n) => {
            out.push(n.as_f64().ok_or("number is not representable as f64")?);
            Ok(())
        }
        Value::Array(items) => items.iter().try_for_each(|v| flatten(v, out)),
        other => Err(format!("expected a number or array, found {other}")),
    }
}

