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

