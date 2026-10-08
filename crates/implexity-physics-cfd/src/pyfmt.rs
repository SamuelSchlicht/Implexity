// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#[must_use]
pub fn fmt_e(x: f64, prec: usize) -> String {
    if let Some(s) = special(x) {
        return s;
    }
    python_exponent(&format!("{x:.prec$e}"))
}

#[must_use]
pub fn fmt_g(x: f64, prec: usize) -> String {
    if let Some(s) = special(x) {
        return s;
    }
    let p = prec.max(1);
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".to_string() } else { "0".to_string() };
    }
    let sci = format!("{x:.prec$e}", prec = p - 1);
    let pos = sci.find('e').unwrap_or(sci.len());
    let exp: i32 = sci[pos + 1..].parse().unwrap_or(0);
    let p_i = i32::try_from(p).unwrap_or(i32::MAX);
    if -4 <= exp && exp < p_i {
        let decimals = usize::try_from(p_i - 1 - exp).unwrap_or(0);
        strip_zeros(&format!("{x:.decimals$}"))
    } else {
        let mant = strip_zeros(&sci[..pos]);
        python_exponent(&format!("{mant}e{}", &sci[pos + 1..]))
    }
}

fn special(x: f64) -> Option<String> {
    if x.is_nan() {
        Some("nan".into())
    } else if x == f64::INFINITY {
        Some("inf".into())
    } else if x == f64::NEG_INFINITY {
        Some("-inf".into())
    } else {
        None
    }
}

fn python_exponent(s: &str) -> String {
    let Some(pos) = s.find('e') else { return s.to_string() };
    let (mant, exp) = s.split_at(pos);
    let exp = &exp[1..];
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(d) => ('-', d),
        None => ('+', exp),
    };
    let digits = if digits.len() < 2 { format!("0{digits}") } else { digits.to_string() };
    format!("{mant}e{sign}{digits}")
}

fn strip_zeros(s: &str) -> String {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s.to_string() }
}

