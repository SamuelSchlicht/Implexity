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

use serde_json::{Number, Value};
use sha2::{Digest, Sha256};

use crate::py_repr::repr_float;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpOptions {
    pub sort_keys: bool,
    pub item_separator: String,
    pub key_separator: String,
    pub ensure_ascii: bool,
    pub indent: Option<String>,
}

impl Default for DumpOptions {
    fn default() -> Self {
        Self {
            sort_keys: false,
            item_separator: ", ".into(),
            key_separator: ": ".into(),
            ensure_ascii: true,
            indent: None,
        }
    }
}

impl DumpOptions {
    #[must_use]
    pub fn canonical() -> Self {
        Self {
            sort_keys: true,
            item_separator: ",".into(),
            key_separator: ":".into(),
            ensure_ascii: true,
            indent: None,
        }
    }

    #[must_use]
    pub fn compact() -> Self {
        Self { sort_keys: false, ..Self::canonical() }
    }

    #[must_use]
    pub fn indented(n: usize) -> Self {
        Self {
            sort_keys: false,
            item_separator: ",".into(),
            key_separator: ": ".into(),
            ensure_ascii: true,
            indent: Some(" ".repeat(n)),
        }
    }

    #[must_use]
    pub fn sorted(mut self, sort_keys: bool) -> Self {
        self.sort_keys = sort_keys;
        self
    }

    #[must_use]
    pub fn ascii(mut self, ensure_ascii: bool) -> Self {
        self.ensure_ascii = ensure_ascii;
        self
    }
}

#[must_use]
pub fn dumps(value: &Value, options: &DumpOptions) -> String {
    let mut out = String::new();
    write_value(&mut out, value, options, 0);
    out
}

#[must_use]
pub fn canonical(value: &Value) -> String {
    dumps(value, &DumpOptions::canonical())
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[must_use]
pub fn canonical_sha256(value: &Value) -> String {
    sha256_hex(canonical(value).as_bytes())
}

#[must_use]
pub fn sha256_of(value: &Value, options: &DumpOptions) -> String {
    sha256_hex(dumps(value, options).as_bytes())
}

#[must_use]
pub fn number_text(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        repr_float(n.as_f64().unwrap_or(f64::NAN))
    }
}

fn newline(out: &mut String, options: &DumpOptions, level: usize) {
    if let Some(indent) = &options.indent {
        out.push('\n');
        for _ in 0..level {
            out.push_str(indent);
        }
    }
}

fn write_value(out: &mut String, value: &Value, options: &DumpOptions, level: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {

            out.push_str(&number_text(n));
        }
        Value::String(s) => write_string(out, s, options.ensure_ascii),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(&options.item_separator);
                }
                newline(out, options, level + 1);
                write_value(out, item, options, level + 1);
            }
            newline(out, options, level);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if options.sort_keys {

                entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            }
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(&options.item_separator);
                }
                newline(out, options, level + 1);
                write_string(out, k, options.ensure_ascii);
                out.push_str(&options.key_separator);
                write_value(out, v, options, level + 1);
            }
            newline(out, options, level);
            out.push('}');
        }
    }
}

pub fn write_string(out: &mut String, s: &str, ensure_ascii: bool) {
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
            c if (c as u32) < 0x20 => push_u_escape(out, c as u32),
            c if ensure_ascii && !(' '..='~').contains(&c) => {
                let code = c as u32;
                if code > 0xffff {
                    let v = code - 0x10000;
                    push_u_escape(out, 0xd800 | ((v >> 10) & 0x3ff));
                    push_u_escape(out, 0xdc00 | (v & 0x3ff));
                } else {
                    push_u_escape(out, code);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_u_escape(out: &mut String, code: u32) {
    let _ = write!(out, "\\u{code:04x}");
}

