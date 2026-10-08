// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

