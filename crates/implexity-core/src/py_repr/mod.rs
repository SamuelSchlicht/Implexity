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




mod printable;

use std::fmt::Write as _;

pub const UNICODE_VERSION: &str = printable::UNICODE_VERSION;

fn split_sci(sci: &str) -> (String, i64) {
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((sci, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let trimmed = digits.trim_end_matches('0');
    let digits = if trimmed.is_empty() { "0".to_string() } else { trimmed.to_string() };
    (digits, exponent.parse().unwrap_or(0))
}

fn shortest_digits(x: f64) -> (String, i64) {
    let shortest = format!("{x:e}");
    let (digits, exp10) = split_sci(&shortest);
    if digits.len() > 1 {
        let rounded = format!("{x:.*e}", digits.len() - 1);
        if rounded.parse::<f64>().is_ok_and(|r| r.to_bits() == x.to_bits()) {
            return split_sci(&rounded);
        }
    }
    (digits, exp10)
}

#[must_use]
pub fn repr_float(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let (digits, exp10) = shortest_digits(x.abs());
    let decpt = exp10 + 1;
    let ndigits = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    let mut out = String::new();
    if x.is_sign_negative() {
        out.push('-');
    }
    if -4 < decpt && decpt <= 16 {
        if decpt <= 0 {
            out.push_str("0.");
            for _ in 0..(-decpt) {
                out.push('0');
            }
            out.push_str(&digits);
        } else if decpt >= ndigits {
            out.push_str(&digits);
            for _ in 0..(decpt - ndigits) {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            let split = usize::try_from(decpt).unwrap_or(0);
            out.push_str(&digits[..split]);
            out.push('.');
            out.push_str(&digits[split..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        let _ = write!(out, "e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    out
}

#[must_use]
pub fn is_printable(c: char) -> bool {
    let code = c as u32;
    if (0x20..0x7f).contains(&code) {
        return true;
    }
    let table = &printable::NON_PRINTABLE;
    table
        .binary_search_by(|&(lo, hi)| {
            if code < lo {
                std::cmp::Ordering::Greater
            } else if code > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_err()
}

#[must_use]
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => {
                let code = c as u32;
                if code < 0x100 {
                    let _ = write!(out, "\\x{code:02x}");
                } else if code < 0x10000 {
                    let _ = write!(out, "\\u{code:04x}");
                } else {
                    let _ = write!(out, "\\U{code:08x}");
                }
            }
        }
    }
    out.push(quote);
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum PyValue {
    None,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Tuple(Vec<PyValue>),
    List(Vec<PyValue>),
    Dict(Vec<(PyValue, PyValue)>),
}

impl PyValue {
    #[must_use]
    pub fn repr(&self) -> String {
        match self {
            Self::None => "None".into(),
            Self::Bool(true) => "True".into(),
            Self::Bool(false) => "False".into(),
            Self::Int(i) => i.to_string(),
            Self::Float(f) => repr_float(*f),
            Self::Str(s) => repr_str(s),
            Self::Tuple(items) => {
                let inner: Vec<String> = items.iter().map(Self::repr).collect();
                if inner.len() == 1 { format!("({},)", inner[0]) } else { format!("({})", inner.join(", ")) }
            }
            Self::List(items) => {
                let inner: Vec<String> = items.iter().map(Self::repr).collect();
                format!("[{}]", inner.join(", "))
            }
            Self::Dict(items) => {
                let inner: Vec<String> =
                    items.iter().map(|(k, v)| format!("{}: {}", k.repr(), v.repr())).collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }

    #[must_use]
    pub fn from_json(value: &serde_json::Value) -> Self {
        use serde_json::Value;
        match value {
            Value::Null => Self::None,
            Value::Bool(b) => Self::Bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Self::Int(i128::from(i))
                } else if let Some(u) = n.as_u64() {
                    Self::Int(i128::from(u))
                } else {
                    Self::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Value::String(s) => Self::Str(s.clone()),
            Value::Array(items) => Self::List(items.iter().map(Self::from_json).collect()),
            Value::Object(map) => {
                Self::Dict(map.iter().map(|(k, v)| (Self::Str(k.clone()), Self::from_json(v))).collect())
            }
        }
    }
}

impl From<f64> for PyValue {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}

impl From<i64> for PyValue {
    fn from(v: i64) -> Self {
        Self::Int(i128::from(v))
    }
}

impl From<bool> for PyValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<&str> for PyValue {
    fn from(v: &str) -> Self {
        Self::Str(v.to_string())
    }
}

impl From<String> for PyValue {
    fn from(v: String) -> Self {
        Self::Str(v)
    }
}

