// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use super::store::DynamicStore;
use super::{DynamicError, DynamicResult};

pub const MAX_SPECTRUM_SAMPLES: usize = 1 << 22;

fn fft_pow2(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let ang = sign * 2.0 * std::f64::consts::PI / len as f64;
        let half = len / 2;

        let tw: Vec<(f64, f64)> =
            (0..half).map(|k| ((ang * k as f64).cos(), (ang * k as f64).sin())).collect();
        for start in (0..n).step_by(len) {
            for (k, &(wr, wi)) in tw.iter().enumerate() {
                let (a, b) = (start + k, start + k + half);
                let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
            }
        }
        len <<= 1;
    }
}



pub fn dft(re: &[f64], im: &[f64]) -> DynamicResult<(Vec<f64>, Vec<f64>)> {
    let n = re.len();
    if im.len() != n || n > MAX_SPECTRUM_SAMPLES {
        return Err(DynamicError::Bound(format!(
            "a spectrum needs matching real/imaginary parts of at most {MAX_SPECTRUM_SAMPLES} samples"
        )));
    }
    if n <= 1 || n.is_power_of_two() {
        let (mut r, mut i) = (re.to_vec(), im.to_vec());
        if n > 1 {
            fft_pow2(&mut r, &mut i, false);
        }
        return Ok((r, i));
    }
    let m = (2 * n - 1).next_power_of_two();
    let chirp: Vec<(f64, f64)> = (0..n)
        .map(|j| {

            let q = ((j as u128 * j as u128) % (2 * n as u128)) as f64;
            let a = -std::f64::consts::PI * q / n as f64;
            (a.cos(), a.sin())
        })
        .collect();
    let (mut ar, mut ai) = (vec![0.0; m], vec![0.0; m]);
    for j in 0..n {
        let (cr, ci) = chirp[j];
        ar[j] = re[j] * cr - im[j] * ci;
        ai[j] = re[j] * ci + im[j] * cr;
    }
    let (mut br, mut bi) = (vec![0.0; m], vec![0.0; m]);
    br[0] = chirp[0].0;
    bi[0] = -chirp[0].1;
    for j in 1..n {
        br[j] = chirp[j].0;
        bi[j] = -chirp[j].1;
        br[m - j] = chirp[j].0;
        bi[m - j] = -chirp[j].1;
    }
    fft_pow2(&mut ar, &mut ai, false);
    fft_pow2(&mut br, &mut bi, false);
    for k in 0..m {
        let (x, y) = (ar[k] * br[k] - ai[k] * bi[k], ar[k] * bi[k] + ai[k] * br[k]);
        ar[k] = x;
        ai[k] = y;
    }
    fft_pow2(&mut ar, &mut ai, true);
    let scale = 1.0 / m as f64;
    let mut out_r = Vec::with_capacity(n);
    let mut out_i = Vec::with_capacity(n);
    for k in 0..n {
        let (cr, ci) = chirp[k];
        let (x, y) = (ar[k] * scale, ai[k] * scale);
        out_r.push(x * cr - y * ci);
        out_i.push(x * ci + y * cr);
    }
    Ok((out_r, out_i))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Rectangular,
    Hann,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spectrum {
    pub frequency: Vec<f64>,
    pub amplitude: Vec<f64>,
    pub psd: Vec<f64>,
    pub resolution: f64,
}



pub fn amplitude_spectrum(x: &[f64], dt: f64, window: Window) -> DynamicResult<Spectrum> {
    let n = x.len();
    if n < 4 || !(dt.is_finite() && dt > 0.0) || x.iter().any(|v| !v.is_finite()) {
        return Err(DynamicError::invalid("a spectrum needs at least 4 finite samples and a positive step"));
    }
    let mean = x.iter().sum::<f64>() / n as f64;
    let w: Vec<f64> = match window {
        Window::Rectangular => vec![1.0; n],
        Window::Hann => {
            (0..n).map(|j| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * j as f64 / n as f64).cos()).collect()
        }
    };
    let sum_w: f64 = w.iter().sum();
    let sum_w2: f64 = w.iter().map(|v| v * v).sum();
    let re: Vec<f64> = x.iter().zip(&w).map(|(v, wj)| (v - mean) * wj).collect();
    let (xr, xi) = dft(&re, &vec![0.0; n])?;
    let half = n / 2;
    let fs = 1.0 / dt;
    let mut frequency = Vec::with_capacity(half + 1);
    let mut amplitude = Vec::with_capacity(half + 1);
    let mut psd = Vec::with_capacity(half + 1);
    for k in 0..=half {
        let mag = xr[k].hypot(xi[k]);
        let edge = k == 0 || (n.is_multiple_of(2) && k == half);
        let factor = if edge { 1.0 } else { 2.0 };
        frequency.push(k as f64 * fs / n as f64);
        amplitude.push(factor * mag / sum_w);
        psd.push(factor * mag * mag / (fs * sum_w2));
    }
    Ok(Spectrum { frequency, amplitude, psd, resolution: fs / n as f64 })
}

#[must_use]
pub fn spectral_peaks(s: &Spectrum, count: usize) -> Vec<(f64, f64)> {
    let a = &s.amplitude;
    let mut peaks: Vec<(f64, f64)> = Vec::new();
    for k in 1..a.len().saturating_sub(1) {
        if a[k] > a[k - 1] && a[k] >= a[k + 1] && a[k] > 0.0 {
            let (l, c, r) = (a[k - 1].max(1e-300).ln(), a[k].ln(), a[k + 1].max(1e-300).ln());
            let den = l - 2.0 * c + r;

            let leaky = a[k - 1] > 1e-6 * a[k] && a[k + 1] > 1e-6 * a[k];
            let delta =
                if leaky && den.abs() > 1e-300 { (0.5 * (l - r) / den).clamp(-0.5, 0.5) } else { 0.0 };
            let amp = (c - 0.25 * (l - r) * delta).exp();
            peaks.push(((k as f64 + delta) * s.resolution, amp));
        }
    }
    peaks.sort_by(|p, q| q.1.total_cmp(&p.1).then(p.0.total_cmp(&q.0)));
    peaks.truncate(count);
    peaks
}

#[must_use]
pub fn upcrossings(t: &[f64], x: &[f64], level: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for j in 1..t.len().min(x.len()) {
        let (a, b) = (x[j - 1] - level, x[j] - level);
        if a < 0.0 && b >= 0.0 && a.is_finite() && b.is_finite() {
            let s = a / (a - b);
            out.push(t[j - 1] + s * (t[j] - t[j - 1]));
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct PeriodEstimate {
    pub period: f64,
    pub jitter: f64,
    pub intervals: usize,
    pub origin: f64,
}



pub fn period_from_crossings(
    t: &[f64],
    x: &[f64],
    level: Option<f64>,
    max_intervals: usize,
) -> DynamicResult<PeriodEstimate> {
    let finite: Vec<f64> = x.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return Err(DynamicError::invalid("the series has no finite samples"));
    }
    let level = level.unwrap_or_else(|| finite.iter().sum::<f64>() / finite.len() as f64);
    let up = upcrossings(t, x, level);
    if up.len() < 2 {
        return Err(DynamicError::invalid(
            "the series crosses its level fewer than twice; no period can be estimated",
        ));
    }
    let intervals: Vec<f64> = up.windows(2).map(|w| w[1] - w[0]).collect();
    let used = &intervals[intervals.len().saturating_sub(max_intervals.max(1))..];
    let period = used.iter().sum::<f64>() / used.len() as f64;
    let jitter = if used.len() > 1 {
        (used.iter().map(|d| (d - period).powi(2)).sum::<f64>() / (used.len() - 1) as f64).sqrt()
    } else {
        0.0
    };
    Ok(PeriodEstimate { period, jitter, intervals: used.len(), origin: *up.last().unwrap_or(&0.0) })
}

#[must_use]
pub fn phase_of(t: f64, period: f64, origin: f64) -> f64 {
    let x = (t - origin) / period;
    (x - x.floor()).clamp(0.0, 1.0 - f64::EPSILON)
}

#[must_use]
pub fn statistics(x: &[f64]) -> Value {
    let f: Vec<f64> = x.iter().copied().filter(|v| v.is_finite()).collect();
    if f.is_empty() {
        return json!({"samples": x.len(), "finite": 0});
    }
    let n = f.len() as f64;
    let mean = f.iter().sum::<f64>() / n;
    let rms = (f.iter().map(|v| v * v).sum::<f64>() / n).sqrt();
    let var = f.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    let min = f.iter().copied().fold(f64::INFINITY, f64::min);
    let max = f.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    json!({"samples": x.len(), "finite": f.len(), "mean": mean, "rms": rms, "std": var.sqrt(),
           "min": min, "max": max, "peak_to_peak": max - min})
}

#[must_use]
pub fn pulse_measures(t: &[f64], x: &[f64], threshold: f64) -> Value {
    let n = t.len().min(x.len());
    if n < 3 {
        return json!({"threshold": threshold, "pulses": 0});
    }
    let span = t[n - 1] - t[0];
    let mut open_time = 0.0;
    for j in 1..n {
        let (a, b) = (x[j - 1] - threshold, x[j] - threshold);
        let dt = t[j] - t[j - 1];
        open_time += match (a > 0.0, b > 0.0) {
            (true, true) => dt,
            (false, false) => 0.0,
            (true, false) => dt * a / (a - b),
            (false, true) => dt * b / (b - a),
        };
    }
    let up = upcrossings(t, x, threshold);
    let neg: Vec<f64> = x.iter().map(|v| -v).collect();
    let down = upcrossings(t, &neg, -threshold);
    let mut durations = Vec::new();
    let mut ratios = Vec::new();
    for &t_open in &up {
        let Some(&t_close) = down.iter().find(|&&d| d > t_open) else { break };
        let (mut peak_t, mut peak_v) = (t_open, f64::NEG_INFINITY);
        for j in 0..n {
            if t[j] > t_open && t[j] < t_close && x[j] > peak_v {
                peak_v = x[j];
                peak_t = t[j];
            }
        }
        durations.push(t_close - t_open);
        if t_close > peak_t && peak_t > t_open {
            ratios.push((peak_t - t_open) / (t_close - peak_t));
        }
    }
    let rates: Vec<f64> =
        (1..n).filter(|&j| t[j] > t[j - 1]).map(|j| (x[j] - x[j - 1]) / (t[j] - t[j - 1])).collect();
    let mean =
        |v: &[f64]| if v.is_empty() { Value::Null } else { json!(v.iter().sum::<f64>() / v.len() as f64) };
    json!({
        "threshold": threshold,
        "open_fraction": if span > 0.0 { json!(open_time / span) } else { Value::Null },
        "pulses": durations.len(),
        "mean_open_duration": mean(&durations),
        "mean_rise_fall_ratio": mean(&ratios),
        "max_rate": rates.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "min_rate": rates.iter().copied().fold(f64::INFINITY, f64::min),
    })
}

#[must_use]
pub fn decimate_minmax(t: &[f64], x: &[f64], max_points: usize) -> (Vec<f64>, Vec<f64>) {
    let n = t.len().min(x.len());
    if n <= max_points || max_points < 4 {
        return (t[..n].to_vec(), x[..n].to_vec());
    }
    let buckets = max_points / 2;
    let (mut ot, mut ox) = (Vec::with_capacity(max_points), Vec::with_capacity(max_points));
    for b in 0..buckets {
        let (lo, hi) = (b * n / buckets, ((b + 1) * n / buckets).max(b * n / buckets + 1));
        let (mut imin, mut imax) = (lo, lo);
        for j in lo..hi.min(n) {
            if x[j] < x[imin] {
                imin = j;
            }
            if x[j] > x[imax] {
                imax = j;
            }
        }
        let (a, c) = if imin <= imax { (imin, imax) } else { (imax, imin) };
        ot.push(t[a]);
        ox.push(x[a]);
        if c != a {
            ot.push(t[c]);
            ox.push(x[c]);
        }
    }
    (ot, ox)
}

#[derive(Clone, Debug, PartialEq)]
pub struct PhaseAverage {
    pub bins: usize,
    pub mean: Vec<Option<Vec<f64>>>,
    pub counts: Vec<usize>,
    pub period: f64,
    pub origin: f64,
}



pub fn phase_average(
    store: &DynamicStore,
    field: &str,
    bins: usize,
    period: Option<f64>,
    origin: Option<f64>,
) -> DynamicResult<PhaseAverage> {
    let (index, _) = store
        .manifest()
        .field(field)
        .ok_or_else(|| DynamicError::invalid(format!("the store has no field {field:?}")))?;
    let period = period
        .or_else(|| store.period())
        .ok_or_else(|| DynamicError::invalid("phase averaging needs a period (none recorded; pass one)"))?;
    if !(period.is_finite() && period > 0.0 && (1..=256).contains(&bins)) {
        return Err(DynamicError::invalid("phase averaging needs a positive period and 1..256 bins"));
    }
    let origin = origin.unwrap_or_else(|| store.phase_origin());
    let values = store.manifest().values_per_frame(index);
    if bins.saturating_mul(values) > 1 << 26 {
        return Err(DynamicError::Bound("the phase average would exceed 2^26 values".into()));
    }
    let mut sums: Vec<Option<Vec<f64>>> = vec![None; bins];
    let mut counts = vec![0usize; bins];
    for f in store.frames() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let b = ((phase_of(f.t, period, origin) * bins as f64) as usize).min(bins - 1);
        let data = store.read_field(f, index)?;
        let acc = sums[b].get_or_insert_with(|| vec![0.0; values]);
        for (a, v) in acc.iter_mut().zip(&data) {
            *a += v;
        }
        counts[b] += 1;
    }
    let mean = sums
        .into_iter()
        .zip(&counts)
        .map(|(s, &c)| s.map(|v| v.into_iter().map(|x| x / c as f64).collect()))
        .collect();
    Ok(PhaseAverage { bins, mean, counts, period, origin })
}

