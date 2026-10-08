// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


fn special(x: f64) -> Option<String> {
    if x.is_nan() {
        Some("nan".into())
    } else if x.is_infinite() {
        Some(if x > 0.0 { "inf".into() } else { "-inf".into() })
    } else {
        None
    }
}

fn fix_exponent(s: &str) -> String {

    let Some(pos) = s.find('e') else { return s.to_string() };
    let (mantissa, exp) = s.split_at(pos);
    let exp = &exp[1..];
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(d) => ('-', d),
        None => ('+', exp),
    };
    let digits = if digits.len() < 2 { format!("0{digits}") } else { digits.to_string() };
    format!("{mantissa}e{sign}{digits}")
}

#[must_use]
pub fn fmt_e(x: f64, prec: usize) -> String {
    if let Some(s) = special(x) {
        return s;
    }
    fix_exponent(&format!("{x:.prec$e}"))
}

#[must_use]
pub fn fmt_g(x: f64, prec: usize) -> String {
    if let Some(s) = special(x) {
        return s;
    }
    let p = prec.max(1);
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{:.*e}", p - 1, x);
    let exp: i32 = sci.find('e').and_then(|i| sci[i + 1..].parse().ok()).unwrap_or(0);
    let p_i = i32::try_from(p).unwrap_or(i32::MAX);
    if exp >= -4 && exp < p_i {
        let decimals = usize::try_from(p_i - 1 - exp).unwrap_or(0);
        let fixed = format!("{x:.decimals$}");
        strip_zeros(&fixed)
    } else {
        let pos = sci.find('e').unwrap_or(sci.len());
        let mantissa = strip_zeros(&sci[..pos]);
        fix_exponent(&format!("{mantissa}{}", &sci[pos..]))
    }
}

fn strip_zeros(s: &str) -> String {
    if s.contains('.') {
        let t = s.trim_end_matches('0');
        t.trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

#[must_use]
pub fn fmt_g6(x: f64) -> String {
    fmt_g(x, 6)
}

#[must_use]
pub fn fmt_e0(x: f64) -> String {
    fmt_e(x, 0)
}

