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



use std::fmt::Write as _;

use serde_json::Value;

use crate::pyfmt::float_repr;

pub fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if ensure_ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_number(out: &mut String, n: &serde_json::Number) {
    if let Some(i) = n.as_i64() {
        let _ = write!(out, "{i}");
    } else if let Some(u) = n.as_u64() {
        let _ = write!(out, "{u}");
    } else if let Some(f) = n.as_f64() {
        out.push_str(&float_repr(f));
    }
}

fn sorted_items(m: &serde_json::Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut items: Vec<(&String, &Value)> = m.iter().collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    items
}

fn write_compact(out: &mut String, v: &Value, ensure_ascii: bool) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_str(out, s, ensure_ascii),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(out, x, ensure_ascii);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, x)) in sorted_items(m).into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k, ensure_ascii);
                out.push(':');
                write_compact(out, x, ensure_ascii);
            }
            out.push('}');
        }
    }
}

fn write_indented(out: &mut String, v: &Value, indent: usize, level: usize) {
    match v {
        Value::Array(a) if !a.is_empty() => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent * (level + 1)));
                write_indented(out, x, indent, level + 1);
            }
            out.push('\n');
            out.push_str(&" ".repeat(indent * level));
            out.push(']');
        }
        Value::Object(m) if !m.is_empty() => {
            out.push('{');
            for (i, (k, x)) in sorted_items(m).into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent * (level + 1)));
                write_str(out, k, true);
                out.push_str(": ");
                write_indented(out, x, indent, level + 1);
            }
            out.push('\n');
            out.push_str(&" ".repeat(indent * level));
            out.push('}');
        }
        other => write_compact(out, other, true),
    }
}

#[must_use]
pub fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write_compact(&mut out, v, true);
    out
}

#[must_use]
pub fn canonical_unicode(v: &Value) -> String {
    let mut out = String::new();
    write_compact(&mut out, v, false);
    out
}

#[must_use]
pub fn indented(v: &Value, indent: usize) -> String {
    let mut out = String::new();
    write_indented(&mut out, v, indent, 0);
    out
}

#[must_use]
pub fn spaced(v: &Value) -> String {
    fn rec(out: &mut String, v: &Value) {
        match v {
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    rec(out, x);
                }
                out.push(']');
            }
            Value::Object(m) => {
                out.push('{');
                for (i, (k, x)) in sorted_items(m).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_str(out, k, true);
                    out.push_str(": ");
                    rec(out, x);
                }
                out.push('}');
            }
            other => write_compact(out, other, true),
        }
    }
    let mut out = String::new();
    rec(&mut out, v);
    out
}

