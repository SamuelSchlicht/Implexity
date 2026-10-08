// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::fmt::Write as _;

#[must_use]
pub fn float_repr(x: f64) -> String {
    implexity_core::py_repr::repr_float(x)
}

#[must_use]
pub fn fmt_g(x: f64, prec: usize) -> String {
    fmt_g_impl(x, prec, false)
}

#[must_use]
pub fn fmt_g_alt(x: f64, prec: usize) -> String {
    fmt_g_impl(x, prec, true)
}

fn fmt_g_impl(x: f64, prec: usize, alt: bool) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let p = prec.max(1);

    let sci = format!("{:.*e}", p - 1, x);
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let p_i = i64::try_from(p).unwrap_or(i64::MAX);
    if exp < -4 || exp >= p_i {
        let mut m = mant.to_string();
        if !alt && m.contains('.') {
            m = m.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{m}e{esign}{:02}", exp.abs());
    }
    let decimals = usize::try_from(p_i - 1 - exp).unwrap_or(0);
    let mut s = format!("{x:.decimals$}");
    if !alt && s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s == "-0" && x == 0.0 {
        return "-0".into();
    }
    s
}

#[must_use]
pub fn g(x: f64) -> String {
    fmt_g(x, 6)
}

#[must_use]
pub fn fmt_f(x: f64, prec: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{x:.prec$}")
}

#[must_use]
pub fn fmt_e(x: f64, prec: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sci = format!("{x:.prec$e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let esign = if exp < 0 { '-' } else { '+' };
    format!("{mant}e{esign}{:02}", exp.abs())
}

#[must_use]
pub fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if (0x80..0xa0).contains(&(c as u32)) => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[must_use]
pub fn shape_str(shape: &[usize]) -> String {
    match shape.len() {
        0 => "()".into(),
        1 => format!("({},)", shape[0]),
        _ => {
            let parts: Vec<String> = shape.iter().map(ToString::to_string).collect();
            format!("({})", parts.join(", "))
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PyObj {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Tuple(Vec<PyObj>),
    List(Vec<PyObj>),
    Dict(Vec<(String, PyObj)>),
}

impl PyObj {
    #[must_use]
    pub fn repr(&self) -> String {
        let mut s = String::new();
        self.write_repr(&mut s);
        s
    }

    fn write_repr(&self, out: &mut String) {
        match self {
            Self::None => out.push_str("None"),
            Self::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
            Self::Int(i) => {
                let _ = write!(out, "{i}");
            }
            Self::Float(f) => out.push_str(&float_repr(*f)),
            Self::Str(s) => out.push_str(&str_repr(s)),
            Self::Tuple(items) => {
                out.push('(');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.write_repr(out);
                }
                if items.len() == 1 {
                    out.push(',');
                }
                out.push(')');
            }
            Self::List(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.write_repr(out);
                }
                out.push(']');
            }
            Self::Dict(items) => {
                out.push('{');
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&str_repr(k));
                    out.push_str(": ");
                    v.write_repr(out);
                }
                out.push('}');
            }
        }
    }

    #[must_use]
    pub fn float_tuple(v: &[f64]) -> Self {
        Self::Tuple(v.iter().map(|x| Self::Float(*x)).collect())
    }

    #[must_use]
    pub fn int_tuple(v: &[i64]) -> Self {
        Self::Tuple(v.iter().map(|x| Self::Int(*x)).collect())
    }
}

#[must_use]
pub fn join_reprs(items: &[String]) -> String {
    items.iter().map(|s| str_repr(s)).collect::<Vec<_>>().join(", ")
}

#[must_use]
pub fn list_repr(items: &[String]) -> String {
    format!("[{}]", join_reprs(items))
}

#[must_use]
pub fn round_half_even(x: f64) -> f64 {
    x.round_ties_even()
}

