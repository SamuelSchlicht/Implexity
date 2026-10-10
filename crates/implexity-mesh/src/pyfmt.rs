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



#[must_use]
pub fn fmt_f(x: f64, prec: usize) -> String {
    if let Some(s) = special(x, false) {
        return s;
    }
    format!("{x:.prec$}")
}

#[must_use]
pub fn fmt_upper_f(x: f64, prec: usize) -> String {
    if let Some(s) = special(x, true) {
        return s;
    }
    format!("{x:.prec$}")
}

#[must_use]
pub fn fmt_e(x: f64, prec: usize) -> String {
    if let Some(s) = special(x, false) {
        return s;
    }
    let s = format!("{x:.prec$e}");
    python_exponent(&s, 'e')
}

#[must_use]
pub fn fmt_g(x: f64, prec: usize) -> String {
    general(x, prec, false)
}

#[must_use]
pub fn fmt_upper_g(x: f64, prec: usize) -> String {
    general(x, prec, true)
}

fn special(x: f64, upper: bool) -> Option<String> {
    let s = if x.is_nan() {
        "nan"
    } else if x == f64::INFINITY {
        "inf"
    } else if x == f64::NEG_INFINITY {
        "-inf"
    } else {
        return None;
    };
    Some(if upper { s.to_uppercase() } else { s.to_string() })
}

fn python_exponent(s: &str, e: char) -> String {
    let Some(pos) = s.find('e') else { return s.to_string() };
    let (mant, exp) = s.split_at(pos);
    let exp = &exp[1..];
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(d) => ('-', d),
        None => ('+', exp),
    };
    let digits = if digits.len() < 2 { format!("0{digits}") } else { digits.to_string() };
    format!("{mant}{e}{sign}{digits}")
}

fn general(x: f64, prec: usize, upper: bool) -> String {
    if let Some(s) = special(x, upper) {
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
        python_exponent(&format!("{mant}e{}", &sci[pos + 1..]), if upper { 'E' } else { 'e' })
    }
}

fn strip_zeros(s: &str) -> String {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s.to_string() }
}

