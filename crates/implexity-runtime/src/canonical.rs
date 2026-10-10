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



use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[must_use]
pub fn float_hex(x: f64) -> String {
    let bits = x.to_bits();
    let sign = if bits >> 63 == 1 { "-" } else { "" };
    let exponent = i64::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
    let mantissa = bits & ((1_u64 << 52) - 1);
    if exponent == 0 && mantissa == 0 {
        return format!("{sign}0x0.0p+0");
    }
    let (lead, exp) = if exponent == 0 { (0, -1022) } else { (1, exponent - 1023) };
    let exp_sign = if exp < 0 { '-' } else { '+' };
    format!("{sign}0x{lead}.{mantissa:013x}p{exp_sign}{}", exp.abs())
}

#[must_use]
pub fn binary64_value(value: &Value) -> Value {
    match value {
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap_or(0.0);
            let f = if f == 0.0 { 0.0 } else { f };
            let mut m = Map::new();
            m.insert("$binary64".into(), Value::String(float_hex(f)));
            Value::Object(m)
        }
        Value::Array(items) => Value::Array(items.iter().map(binary64_value).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), binary64_value(v))).collect())
        }
        other => other.clone(),
    }
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[must_use]
pub fn canonical_text(value: &Value) -> String {
    implexity_core::json::canonical(value)
}

#[must_use]
pub fn canonical_sha256(value: &Value) -> String {
    sha256_hex(canonical_text(value).as_bytes())
}

#[must_use]
pub fn evidence_sha256(value: &Value) -> String {
    canonical_sha256(&binary64_value(value))
}

#[must_use]
pub fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[must_use]
pub fn is_digest_value(value: &Value) -> bool {
    value.as_str().is_some_and(is_digest)
}

#[must_use]
pub fn is_canonical_text(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && !value.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
        && unicode_normalization::is_nfc(value)
}

#[must_use]
pub fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (k, v) in pairs {
        map.insert(k.to_string(), v);
    }
    Value::Object(map)
}

#[must_use]
pub fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

#[must_use]
pub fn opt_s(value: Option<&str>) -> Value {
    value.map_or(Value::Null, s)
}

#[must_use]
pub fn f(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

#[must_use]
pub fn opt_f(value: Option<f64>) -> Value {
    value.map_or(Value::Null, f)
}

#[must_use]
pub fn str_array<S: AsRef<str>>(items: &[S]) -> Value {
    Value::Array(items.iter().map(|i| Value::String(i.as_ref().to_string())).collect())
}

#[must_use]
pub fn has_exact_keys(map: &Map<String, Value>, expected: &[&str]) -> bool {
    map.len() == expected.len() && expected.iter().all(|k| map.contains_key(*k))
}

