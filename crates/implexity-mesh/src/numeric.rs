// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#[must_use]
pub fn py_sum<I: IntoIterator<Item = f64>>(items: I) -> f64 {
    let (mut f, mut c) = (0.0_f64, 0.0_f64);
    for x in items {
        let t = f + x;
        if f.abs() >= x.abs() {
            c += (f - t) + x;
        } else {
            c += (x - t) + f;
        }
        f = t;
    }
    if c != 0.0 && c.is_finite() {
        f += c;
    }
    f
}

#[must_use]
pub fn py_hypot(v: &[f64]) -> f64 {
    let mut vec: Vec<f64> = v.iter().map(|x| x.abs()).collect();
    let found_nan = vec.iter().any(|x| x.is_nan());
    let max = vec.iter().copied().filter(|x| !x.is_nan()).fold(0.0_f64, f64::max);
    vector_norm(&mut vec, max, found_nan)
}

fn vector_norm(vec: &mut [f64], max: f64, found_nan: bool) -> f64 {
    if max.is_infinite() {
        return max;
    }
    if found_nan {
        return f64::NAN;
    }
    if max == 0.0 || vec.len() <= 1 {
        return max;
    }
    let max_e = frexp_exponent(max);
    if max_e < -1023 {
        for x in vec.iter_mut() {
            *x /= f64::MIN_POSITIVE;
        }
        return f64::MIN_POSITIVE * vector_norm(vec, max / f64::MIN_POSITIVE, found_nan);
    }
    let scale = ldexp1(-max_e);
    let (mut csum, mut frac1, mut frac2) = (1.0_f64, 0.0_f64, 0.0_f64);
    let fast_sum = |a: f64, b: f64| {
        let x = a + b;
        (x, (a - x) + b)
    };
    for &x in vec.iter() {
        let x = x * scale;
        let hi = x * x;
        let lo = x.mul_add(x, -hi);
        let (s, e) = fast_sum(csum, hi);
        csum = s;
        frac1 += lo;
        frac2 += e;
    }
    let mut h = (csum - 1.0 + (frac1 + frac2)).sqrt();
    let hi = -h * h;
    let lo = (-h).mul_add(h, -hi);
    let (s, e) = fast_sum(csum, hi);
    csum = s;
    frac1 += lo;
    frac2 += e;
    let x = csum - 1.0 + (frac1 + frac2);
    h += x / (2.0 * h);
    h / scale
}

fn frexp_exponent(x: f64) -> i32 {
    let bits = x.to_bits();
    let biased = i32::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
    if biased == 0 {

        return frexp_exponent(x * ldexp1(64)) - 64;
    }
    biased - 1022
}

fn ldexp1(e: i32) -> f64 {
    let biased = u64::try_from(e + 1023).unwrap_or(0);
    f64::from_bits(biased << 52)
}

#[must_use]
pub fn pairwise_sum(a: &[f64]) -> f64 {
    const BLOCK: usize = 128;
    let n = a.len();
    if n < 8 {
        let mut res = -0.0_f64;
        for &v in a {
            res += v;
        }
        if n == 0 {
            return 0.0;
        }
        return res;
    }
    if n <= BLOCK {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - (n % 8) {
            for k in 0..8 {
                r[k] += a[i + k];
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
    pairwise_sum(&a[..n2]) + pairwise_sum(&a[n2..])
}

#[must_use]
pub fn mean(a: &[f64]) -> f64 {
    if a.is_empty() {
        return f64::NAN;
    }
    pairwise_sum(a) / a.len() as f64
}

#[must_use]
pub fn linspace(start: f64, stop: f64, num: usize) -> Vec<f64> {
    if num == 0 {
        return Vec::new();
    }
    if num == 1 {
        return vec![start];
    }
    let div = (num - 1) as f64;
    let delta = stop - start;
    let step = delta / div;
    let mut out: Vec<f64> = if step == 0.0 {
        (0..num).map(|i| (i as f64 / div) * delta + start).collect()
    } else {
        (0..num).map(|i| i as f64 * step + start).collect()
    };
    out[num - 1] = stop;
    out
}

#[must_use]
pub fn searchsorted(sorted: &[f64], value: f64, right: bool) -> usize {
    if right { sorted.partition_point(|&t| t <= value) } else { sorted.partition_point(|&t| t < value) }
}

#[must_use]
pub fn np_lerp(a: f64, b: f64, t: f64) -> f64 {
    let diff = b - a;
    if t >= 0.5 { b - diff * (1.0 - t) } else { a + diff * t }
}

#[must_use]
pub fn percentile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let virtual_index = q / 100.0 * (n - 1) as f64;
    let lo = virtual_index.floor();
    let t = virtual_index - lo;
    let i = crate::cast::trunc_usize(lo).min(n - 1);
    let j = (i + 1).min(n - 1);
    np_lerp(v[i], v[j], t)
}

#[must_use]
pub fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {

        #[allow(clippy::manual_midpoint)]
        let m = (v[n / 2 - 1] + v[n / 2]) / 2.0;
        m
    }
}

#[must_use]
pub fn interp(x: f64, xp: &[f64], fp: &[f64]) -> f64 {
    let n = xp.len();
    if n == 0 {
        return f64::NAN;
    }
    if x <= xp[0] {
        return fp[0];
    }
    if x >= xp[n - 1] {
        return fp[n - 1];
    }
    let j = searchsorted(xp, x, true) - 1;
    #[allow(clippy::float_cmp)]
    if x == xp[j] {
        return fp[j];
    }
    let slope = (fp[j + 1] - fp[j]) / (xp[j + 1] - xp[j]);
    let r = slope * (x - xp[j]) + fp[j];
    if r.is_nan() {
        let r2 = slope * (x - xp[j + 1]) + fp[j + 1];
        #[allow(clippy::float_cmp)]
        let same = fp[j] == fp[j + 1];
        if r2.is_nan() && same { fp[j] } else { r2 }
    } else {
        r
    }
}

#[must_use]
pub fn gradient3(data: &[f64], shape: [usize; 3], h: f64, axis: usize) -> Vec<f64> {
    let mut out = vec![0.0; data.len()];
    let n = shape[axis];
    if n < 2 {
        return out;
    }
    let strides = [shape[1] * shape[2], shape[2], 1];
    let s = strides[axis];
    for (idx, o) in out.iter_mut().enumerate() {
        let pos = (idx / s) % n;
        *o = if pos == 0 {
            (data[idx + s] - data[idx]) / h
        } else if pos == n - 1 {
            (data[idx] - data[idx - s]) / h
        } else {
            (data[idx + s] - data[idx - s]) / (2.0 * h)
        };
    }
    out
}

#[must_use]
#[inline]
pub fn clip(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        return v;
    }
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

#[must_use]
#[inline]
pub fn to_u8(v: f64) -> u8 {
    if v.is_nan() || v <= 0.0 {
        0
    } else if v >= 255.0 {
        255
    } else {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let b = v as u8;
        b
    }
}

#[must_use]
pub fn py_round(v: f64) -> f64 {
    v.round_ties_even()
}

#[must_use]
pub fn py_round_digits(v: f64, ndigits: usize) -> f64 {
    if !v.is_finite() {
        return v;
    }
    format!("{v:.ndigits$}").parse().unwrap_or(v)
}

