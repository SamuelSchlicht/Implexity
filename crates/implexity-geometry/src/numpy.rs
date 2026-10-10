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


const PW_BLOCKSIZE: usize = 128;

#[must_use]
pub fn sum(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut r = -0.0;
        for x in a {
            r += x;
        }
        return r;
    }
    if n <= PW_BLOCKSIZE {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - (n % 8) {
            for j in 0..8 {
                r[j] += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        return res;
    }
    let mut n2 = n / 2;
    n2 -= n2 % 8;
    sum(&a[..n2]) + sum(&a[n2..])
}

#[must_use]
pub fn mean(a: &[f64]) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = a.len() as f64;
    sum(a) / n
}

#[must_use]
pub fn percentile(a: &[f64], q: f64) -> Option<f64> {
    if a.is_empty() {
        return None;
    }
    let mut s: Vec<f64> = a.to_vec();
    s.sort_by(f64::total_cmp);
    if s.iter().any(|x| x.is_nan()) {
        return Some(f64::NAN);
    }
    let n = s.len();
    #[allow(clippy::cast_precision_loss)]
    let vi = (q / 100.0) * (n - 1) as f64;
    let prev = vi.floor();
    let gamma = vi - prev;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lo = (prev.max(0.0) as usize).min(n - 1);
    let hi = (lo + 1).min(n - 1);
    let (x0, x1) = (s[lo], s[hi]);
    let diff = x1 - x0;
    Some(if gamma >= 0.5 { x1 - diff * (1.0 - gamma) } else { x0 + diff * gamma })
}

#[must_use]
pub fn linspace(start: f64, stop: f64, num: usize) -> Vec<f64> {
    if num == 0 {
        return Vec::new();
    }
    if num == 1 {
        return vec![start];
    }
    #[allow(clippy::cast_precision_loss)]
    let div = (num - 1) as f64;
    let delta = stop - start;
    let step = delta / div;
    let mut out: Vec<f64> = if step == 0.0 {
        #[allow(clippy::cast_precision_loss)]
        (0..num).map(|i| (i as f64 / div) * delta + start).collect()
    } else {
        #[allow(clippy::cast_precision_loss)]
        (0..num).map(|i| i as f64 * step + start).collect()
    };
    out[num - 1] = stop;
    out
}

#[must_use]
pub fn max(a: &[f64]) -> Option<f64> {
    let mut it = a.iter();
    let mut m = *it.next()?;
    for x in it {
        if x.is_nan() || m.is_nan() {
            m = f64::NAN;
        } else if *x > m {
            m = *x;
        }
    }
    Some(m)
}

#[must_use]
pub fn min(a: &[f64]) -> Option<f64> {
    let mut it = a.iter();
    let mut m = *it.next()?;
    for x in it {
        if x.is_nan() || m.is_nan() {
            m = f64::NAN;
        } else if *x < m {
            m = *x;
        }
    }
    Some(m)
}

#[must_use]
pub fn norm(a: &[f64]) -> f64 {
    let Some((first, rest)) = a.split_first() else { return 0.0 };
    let mut s = first * first;
    for x in rest {
        s = x.mul_add(*x, s);
    }
    s.sqrt()
}


