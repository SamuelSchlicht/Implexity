// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::f64::consts::PI;

use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;

#[derive(Clone, Debug, PartialEq)]
pub enum TimeWindow {
    Periodic,
    Uniform {
        from: usize,
        to: usize,
    },
    Trapezoid {
        from: usize,
        to: usize,
    },
    Hann {
        from: usize,
        to: usize,
    },
    Bump {
        from: usize,
        to: usize,
    },
    Tukey {
        from: usize,
        to: usize,
        alpha: f64,
    },
    Custom(Vec<f64>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarmonicPart {
    Amplitude,
    Real,
    Imag,
}

impl HarmonicPart {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Amplitude => "amplitude",
            Self::Real => "real",
            Self::Imag => "imag",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SeriesFunctional {
    Mean {
        sample: usize,
    },
    MeanSquare {
        sample: usize,
    },
    Rms {
        sample: usize,
    },
    Variance {
        sample: usize,
    },
    SmoothMax {
        sample: usize,
        beta: f64,
    },
    SmoothMin {
        sample: usize,
        beta: f64,
    },
    SmoothPeakToPeak {
        sample: usize,
        beta: f64,
    },
    Harmonic {
        sample: usize,
        order: usize,
        part: HarmonicPart,
    },
    HarmonicFit { sample: usize, order: usize, part: HarmonicPart },
    BandPower {
        sample: usize,
        lo: usize,
        hi: usize,
    },
    CrossingPeriod {
        sample: usize,
        level: f64,
    },
    DutyFraction {
        sample: usize,
        level: f64,
        width: f64,
    },
    Rate {
        inner: Box<SeriesFunctional>,
    },

    WaveformMismatch {
        sample: usize,
        reference: Vec<f64>,
        period_rows: Option<usize>,
        normalise: bool,
        beta: f64,
    },
}

impl SeriesFunctional {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Mean { .. } => "mean",
            Self::MeanSquare { .. } => "mean_square",
            Self::Rms { .. } => "rms",
            Self::Variance { .. } => "variance",
            Self::SmoothMax { .. } => "smooth_max",
            Self::SmoothMin { .. } => "smooth_min",
            Self::SmoothPeakToPeak { .. } => "smooth_peak_to_peak",
            Self::Harmonic { .. } => "harmonic",
            Self::HarmonicFit { .. } => "harmonic_fit",
            Self::BandPower { .. } => "band_power",
            Self::CrossingPeriod { .. } => "crossing_period",
            Self::DutyFraction { .. } => "duty_fraction",
            Self::Rate { .. } => "rate",
            Self::WaveformMismatch { .. } => "waveform_mismatch",
        }
    }

    #[must_use]
    pub fn sample(&self) -> usize {
        match self {
            Self::Mean { sample }
            | Self::MeanSquare { sample }
            | Self::Rms { sample }
            | Self::Variance { sample }
            | Self::SmoothMax { sample, .. }
            | Self::SmoothMin { sample, .. }
            | Self::SmoothPeakToPeak { sample, .. }
            | Self::Harmonic { sample, .. }
            | Self::HarmonicFit { sample, .. }
            | Self::BandPower { sample, .. }
            | Self::CrossingPeriod { sample, .. }
            | Self::DutyFraction { sample, .. }
            | Self::WaveformMismatch { sample, .. } => *sample,
            Self::Rate { inner } => inner.sample(),
        }
    }

    #[must_use]
    pub fn with_sample(&self, sample: usize) -> Self {
        let mut out = self.clone();
        match &mut out {
            Self::Mean { sample: s }
            | Self::MeanSquare { sample: s }
            | Self::Rms { sample: s }
            | Self::Variance { sample: s }
            | Self::SmoothMax { sample: s, .. }
            | Self::SmoothMin { sample: s, .. }
            | Self::SmoothPeakToPeak { sample: s, .. }
            | Self::Harmonic { sample: s, .. }
            | Self::HarmonicFit { sample: s, .. }
            | Self::BandPower { sample: s, .. }
            | Self::CrossingPeriod { sample: s, .. }
            | Self::DutyFraction { sample: s, .. }
            | Self::WaveformMismatch { sample: s, .. } => *s = sample,
            Self::Rate { inner } => **inner = inner.with_sample(sample),
        }
        out
    }


    pub fn validate(&self) -> CaeResult<()> {
        let finite = |value: f64, what: &str| {
            if value.is_finite() {
                Ok(())
            } else {
                Err(CaeError::contract(format!("{} {what} must be a finite real number", self.kind())))
            }
        };
        match self {
            Self::SmoothMax { beta, .. }
            | Self::SmoothMin { beta, .. }
            | Self::SmoothPeakToPeak { beta, .. } => {
                if beta.is_finite() && *beta > 0.0 {
                    Ok(())
                } else {
                    Err(CaeError::contract(format!("{} beta must be a finite positive number", self.kind())))
                }
            }
            Self::Harmonic { order, .. } | Self::HarmonicFit { order, .. } => {
                if *order >= 1 {
                    Ok(())
                } else {
                    Err(CaeError::contract(
                        "harmonic order must be at least 1 (the mean is the order-0 coefficient)",
                    ))
                }
            }
            Self::BandPower { lo, hi, .. } => {
                if lo <= hi {
                    Ok(())
                } else {
                    Err(CaeError::contract("band_power requires lo <= hi"))
                }
            }
            Self::CrossingPeriod { level, .. } => finite(*level, "level"),
            Self::DutyFraction { level, width, .. } => {
                finite(*level, "level")?;
                if width.is_finite() && *width > 0.0 {
                    Ok(())
                } else {
                    Err(CaeError::contract("duty_fraction width must be a finite positive number"))
                }
            }
            Self::Rate { inner } => inner.validate(),
            Self::WaveformMismatch { reference, period_rows, beta, .. } => {
                if reference.len() < 3 || reference.iter().any(|r| !r.is_finite()) {
                    return Err(CaeError::contract(
                        "waveform_mismatch reference must list at least three finite values over one period",
                    ));
                }
                let (lo, hi) = reference
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), r| (a.min(*r), b.max(*r)));
                if hi - lo <= 64.0 * f64::EPSILON * hi.abs().max(lo.abs()) {
                    return Err(CaeError::contract(
                        "waveform_mismatch reference is constant; it has no waveform to align with",
                    ));
                }
                if period_rows.is_some_and(|p| p < 3) {
                    return Err(CaeError::contract("waveform_mismatch period_rows must be at least 3"));
                }
                if beta.is_finite() && *beta > 0.0 {
                    Ok(())
                } else {
                    Err(CaeError::contract("waveform_mismatch beta must be a finite positive number"))
                }
            }
            Self::Mean { .. } | Self::MeanSquare { .. } | Self::Rms { .. } | Self::Variance { .. } => Ok(()),
        }
    }

    #[must_use]
    pub fn depends_on_step(&self) -> bool {
        matches!(self, Self::Rate { .. } | Self::CrossingPeriod { .. })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FunctionalValue {
    pub value: f64,
    pub d_samples: DenseMatrix,
    pub d_period_s: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowSupport {
    pub from: usize,
    pub weights: Vec<f64>,
    pub base: usize,
    pub cyclic: bool,
}

impl WindowSupport {
    #[must_use]
    pub fn to(&self) -> usize {
        self.from + self.weights.len()
    }

    #[must_use]
    pub fn weight(&self, i: usize) -> f64 {
        if i >= self.from && i < self.to() { self.weights[i - self.from] } else { 0.0 }
    }
}

const AUTONOMOUS_WINDOW_REFUSAL: &str = "over an autonomous oscillation, a fixed-horizon average whose window \
     ends do not vanish has a gradient with an O(1) phase-drift term that does not decay with the window length \
     (windowing theorem, Krakos-Wang-Hall-Darmofal 2012); use a smooth window (hann, bump, tukey) or the \
     periodic window of a periodic-orbit solve";

fn normalised(raw: Vec<f64>, kind: &str) -> CaeResult<Vec<f64>> {
    let sum: f64 = raw.iter().sum();
    if sum.is_finite() && sum > 0.0 {
        Ok(raw.into_iter().map(|w| w / sum).collect())
    } else {
        Err(CaeError::contract(format!("{kind} time window has no positive weight")))
    }
}

impl TimeWindow {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Periodic => "periodic",
            Self::Uniform { .. } => "uniform",
            Self::Trapezoid { .. } => "trapezoid",
            Self::Hann { .. } => "hann",
            Self::Bump { .. } => "bump",
            Self::Tukey { .. } => "tukey",
            Self::Custom(_) => "custom",
        }
    }

    #[must_use]
    pub fn range(&self) -> Option<(usize, usize)> {
        match self {
            Self::Uniform { from, to }
            | Self::Trapezoid { from, to }
            | Self::Hann { from, to }
            | Self::Bump { from, to }
            | Self::Tukey { from, to, .. } => Some((*from, *to)),
            Self::Periodic | Self::Custom(_) => None,
        }
    }

    #[must_use]
    pub fn is_smooth(&self) -> bool {
        matches!(self, Self::Hann { .. } | Self::Bump { .. } | Self::Tukey { .. })
    }

    #[must_use]
    pub fn half_window(&self) -> Option<Self> {
        let (from, to) = self.range()?;
        let half = to.saturating_sub(from) / 2;
        if half == 0 {
            return None;
        }
        let from = to - half;
        Some(match self {
            Self::Uniform { .. } => Self::Uniform { from, to },
            Self::Trapezoid { .. } => Self::Trapezoid { from, to },
            Self::Hann { .. } => Self::Hann { from, to },
            Self::Bump { .. } => Self::Bump { from, to },
            Self::Tukey { alpha, .. } => Self::Tukey { from, to, alpha: *alpha },
            Self::Periodic | Self::Custom(_) => return None,
        })
    }


    pub fn validate(&self) -> CaeResult<()> {
        if let Some((from, to)) = self.range()
            && from >= to
        {
            return Err(CaeError::contract(format!(
                "{} time window rows {from}..{to} must satisfy from < to",
                self.kind()
            )));
        }
        match self {
            Self::Trapezoid { from, to } if to - from < 2 => {
                Err(CaeError::contract("trapezoid time window needs at least two rows"))
            }
            Self::Tukey { alpha, .. } if !(alpha.is_finite() && *alpha > 0.0 && *alpha <= 1.0) => {
                Err(CaeError::contract("tukey time window alpha must lie in (0, 1]"))
            }
            Self::Custom(weights) => {
                if weights.is_empty() {
                    return Err(CaeError::contract("custom time window needs one weight per sample row"));
                }
                if weights.iter().any(|w| !(w.is_finite() && *w >= 0.0)) {
                    return Err(CaeError::contract(
                        "custom time window weights must be finite and non-negative",
                    ));
                }
                if weights.iter().sum::<f64>() > 0.0 {
                    Ok(())
                } else {
                    Err(CaeError::contract("custom time window has no positive weight"))
                }
            }
            _ => Ok(()),
        }
    }


    pub fn support(&self, rows: usize, periodic: bool, autonomous: bool) -> CaeResult<WindowSupport> {
        self.validate()?;
        if rows == 0 {
            return Err(CaeError::contract("a time functional needs at least one sample row"));
        }
        if autonomous && !periodic && !self.is_smooth() {
            return Err(CaeError::contract(format!(
                "{} time window refused: {AUTONOMOUS_WINDOW_REFUSAL}",
                self.kind()
            )));
        }
        if let Some((_, to)) = self.range()
            && to > rows
        {
            return Err(CaeError::contract(format!(
                "{} time window ends at row {to} but the series has {rows} rows",
                self.kind()
            )));
        }
        let span = |from: usize, to: usize, weights: Vec<f64>| -> WindowSupport {
            WindowSupport { from, weights, base: to - from, cyclic: false }
        };
        let theta = |j: usize, m: usize| (j + 1) as f64 / (m + 1) as f64;
        Ok(match self {
            Self::Periodic => {
                if !periodic {
                    return Err(CaeError::contract(
                        "the periodic time window needs the samples of exactly one period of a periodic orbit",
                    ));
                }
                WindowSupport { from: 0, weights: vec![1.0 / rows as f64; rows], base: rows, cyclic: true }
            }
            Self::Uniform { from, to } => {
                let m = to - from;
                span(*from, *to, vec![1.0 / m as f64; m])
            }
            Self::Trapezoid { from, to } => {
                let m = to - from;
                let interior = (m - 1) as f64;
                let weights = (0..m)
                    .map(|j| if j == 0 || j == m - 1 { 0.5 / interior } else { 1.0 / interior })
                    .collect();
                span(*from, *to, weights)
            }
            Self::Hann { from, to } => {
                let m = to - from;
                let raw = (0..m).map(|j| (PI * theta(j, m)).sin().powi(2)).collect();
                span(*from, *to, normalised(raw, "hann")?)
            }
            Self::Bump { from, to } => {
                let m = to - from;
                let raw = (0..m)
                    .map(|j| {
                        let t = theta(j, m);
                        (4.0 - 1.0 / (t * (1.0 - t))).exp()
                    })
                    .collect();
                span(*from, *to, normalised(raw, "bump")?)
            }
            Self::Tukey { from, to, alpha } => {
                let m = to - from;
                let raw = (0..m)
                    .map(|j| {
                        let t = theta(j, m);
                        let edge = t.min(1.0 - t);
                        if edge < 0.5 * alpha { 0.5 * (1.0 - (2.0 * PI * edge / alpha).cos()) } else { 1.0 }
                    })
                    .collect();
                span(*from, *to, normalised(raw, "tukey")?)
            }
            Self::Custom(weights) => {
                if weights.len() != rows {
                    return Err(CaeError::contract(format!(
                        "custom time window has {} weights but the series has {rows} rows",
                        weights.len()
                    )));
                }
                WindowSupport {
                    from: 0,
                    weights: normalised(weights.clone(), "custom")?,
                    base: rows,
                    cyclic: false,
                }
            }
        })
    }
}

fn phase(k: usize, row: usize, base: usize) -> CaeResult<f64> {
    let turns = k
        .checked_mul(row + 1)
        .ok_or_else(|| CaeError::contract("harmonic order times row index overflows"))?;
    Ok(2.0 * PI * (turns % base) as f64 / base as f64)
}

fn check_harmonic_order(k: usize, base: usize, what: &str) -> CaeResult<()> {
    if k.checked_mul(2).is_some_and(|two_k| two_k < base) {
        Ok(())
    } else {
        Err(CaeError::contract(format!(
            "{what} order {k} needs more than {} rows in the window span ({base} given)",
            2 * k
        )))
    }
}

fn smooth_step(u: f64) -> (f64, f64) {
    if u <= 0.0 {
        (0.0, 0.0)
    } else if u >= 1.0 {
        (1.0, 0.0)
    } else {
        (u * u * u * (10.0 + u * (-15.0 + 6.0 * u)), 30.0 * u * u * (1.0 - u) * (1.0 - u))
    }
}

fn on_level(s: f64, level: f64) -> bool {
    (s - level).abs() <= 16.0 * f64::EPSILON * s.abs().max(level.abs())
}

fn crossings(s: &[f64], support: &WindowSupport, level: f64) -> CaeResult<Vec<(usize, usize)>> {
    let (from, to) = (support.from, support.to());
    if let Some(i) = (from..to).find(|&i| on_level(s[i], level)) {
        return Err(CaeError::convergence(format!(
            "crossing_period: sample row {i} lies on the level {level}, so the crossing is not transversal and the \
             crossing count is not differentiable"
        )));
    }
    let mut pairs: Vec<(usize, usize)> = (from..to.saturating_sub(1)).map(|i| (i, i + 1)).collect();
    if support.cyclic && to - from >= 2 {
        pairs.push((to - 1, from));
    }
    Ok(pairs.into_iter().filter(|&(i, j)| s[i] < level && s[j] > level).collect())
}

fn rate_rows_check(rows: usize) -> CaeResult<()> {
    if rows >= 3 { Ok(()) } else { Err(CaeError::contract("rate needs at least three sample rows")) }
}

fn rate_series<S: Scalar>(s: &[S], dt: S, periodic: bool) -> Vec<S> {
    let n = s.len();
    let two_dt = dt * 2.0;
    (0..n)
        .map(|i| {
            if periodic {
                (s[(i + 1) % n] - s[(i + n - 1) % n]) / two_dt
            } else if i == 0 {
                (s[1] * 4.0 - s[0] * 3.0 - s[2]) / two_dt
            } else if i == n - 1 {
                (s[n - 1] * 3.0 - s[n - 2] * 4.0 + s[n - 3]) / two_dt
            } else {
                (s[i + 1] - s[i - 1]) / two_dt
            }
        })
        .collect()
}

fn rate_transpose(g: &[f64], dt: f64, periodic: bool) -> Vec<f64> {
    let n = g.len();
    let c = 1.0 / (2.0 * dt);
    let mut out = vec![0.0; n];
    for (i, &gi) in g.iter().enumerate() {
        if gi == 0.0 {
            continue;
        }
        let a = gi * c;
        if periodic {
            out[(i + 1) % n] += a;
            out[(i + n - 1) % n] -= a;
        } else if i == 0 {
            out[1] += 4.0 * a;
            out[0] -= 3.0 * a;
            out[2] -= a;
        } else if i == n - 1 {
            out[n - 1] += 3.0 * a;
            out[n - 2] -= 4.0 * a;
            out[n - 3] += a;
        } else {
            out[i + 1] += a;
            out[i - 1] -= a;
        }
    }
    out
}

fn waveform_reference(reference: &[f64], p: usize, normalise: bool) -> CaeResult<(Vec<f64>, f64)> {
    let m = reference.len();
    let r: Vec<f64> = (0..p)
        .map(|k| {
            let x = k as f64 * m as f64 / p as f64;
            let j = x.floor();
            let f = x - j;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let j = (j as usize) % m;
            reference[j] * (1.0 - f) + reference[(j + 1) % m] * f
        })
        .collect();
    let mean = r.iter().sum::<f64>() / p as f64;
    let var = r.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / p as f64;
    let scale = r.iter().map(|x| x.abs()).fold(0.0, f64::max);
    if var <= (64.0 * f64::EPSILON * scale).powi(2) {
        return Err(CaeError::contract(format!(
            "waveform_mismatch reference resampled onto {p} rows per period is constant; use more rows per period"
        )));
    }
    Ok(if normalise { (r.iter().map(|x| (x - mean) / var.sqrt()).collect(), 1.0) } else { (r, var) })
}

fn waveform_period(period_rows: Option<usize>, sup: &WindowSupport) -> CaeResult<usize> {
    match period_rows {
        Some(p) => Ok(p),
        None if sup.cyclic => Ok(sup.base),
        None => Err(CaeError::contract(
            "waveform_mismatch over a range window needs period_rows (the rows of one period of the series)",
        )),
    }
}

fn waveform_index(l: usize, tau: usize, p: usize) -> usize {
    (l + 1 + tau) % p
}

struct ColumnGrad {
    value: f64,
    d_column: Vec<f64>,
    d_step: f64,
}

fn smooth_max_grad(s: &[f64], sup: &WindowSupport, beta: f64, sign: f64) -> CaeResult<(f64, Vec<f64>)> {
    let rows = sup.from..sup.to();
    let shift =
        rows.clone().filter(|&i| sup.weight(i) > 0.0).map(|i| sign * s[i]).fold(f64::NEG_INFINITY, f64::max);
    if !shift.is_finite() {
        return Err(CaeError::contract("smooth maximum over a window without positive weight"));
    }
    let mut terms = vec![0.0; s.len()];
    let mut total = 0.0;
    for i in rows {
        let e = sup.weight(i) * (beta * (sign * s[i] - shift)).exp();
        terms[i] = e;
        total += e;
    }
    let value = shift + total.ln() / beta;
    for t in &mut terms {
        *t *= sign / total;
    }
    Ok((value, terms))
}

fn harmonic_fit<S: Scalar>(s: &[S], sup: &WindowSupport, k: usize) -> CaeResult<(S, S, Vec<[f64; 2]>)> {
    check_harmonic_order(k, sup.base, "harmonic_fit")?;
    let mut g = DenseMatrix::zeros(3, 3);
    let mut basis = Vec::with_capacity(sup.weights.len());
    for i in sup.from..sup.to() {
        let theta = phase(k, i, sup.base)?;
        let b = [1., theta.cos(), theta.sin()];
        for r in 0..3 { for c in 0..3 { g.data[r*3+c] += sup.weight(i)*b[r]*b[c]; } }
        basis.push(b);
    }
    let lu = implexity_linalg::dense::DenseLu::new(&g).map_err(|e| CaeError::convergence(format!("harmonic_fit basis: {e}")))?;
    let inverse = lu.solve(&[1.,0.,0.,0.,1.,0.,0.,0.,1.], 3, false).map_err(|e| CaeError::convergence(format!("harmonic_fit basis: {e}")))?;
    let norm = |a: &[f64]| (0..3).map(|c| (0..3).map(|r| a[r*3+c].abs()).sum::<f64>()).fold(0., f64::max);
    let condition = norm(&g.data)*norm(&inverse);
    if !condition.is_finite() || condition*128.*f64::EPSILON >= 1. { return Err(CaeError::convergence("harmonic_fit basis is numerically rank deficient")); }
    let mut map = vec![[0.;2]; s.len()];
    let anchor = s[sup.from];
    let (mut re, mut im) = (S::zero(), S::zero());
    let mut sum = [0.;2];
    for (j,b) in basis.iter().enumerate() {
        let i = sup.from+j;
        map[i] = [sup.weight(i)*(0..3).map(|c| inverse[3+c]*b[c]).sum::<f64>(), -sup.weight(i)*(0..3).map(|c| inverse[6+c]*b[c]).sum::<f64>()];
        re += (s[i]-anchor)*map[i][0]; im += (s[i]-anchor)*map[i][1];
        sum[0] += map[i][0]; sum[1] += map[i][1];
    }
    map[sup.from][0] -= sum[0]; map[sup.from][1] -= sum[1];
    if !re.value().is_finite() || !im.value().is_finite() { return Err(CaeError::convergence("harmonic_fit value overflow")); }
    Ok((re, im, map))
}

fn harmonic_coefficient(s: &[f64], sup: &WindowSupport, k: usize) -> CaeResult<(f64, f64)> {
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &si) in s.iter().enumerate().take(sup.to()).skip(sup.from) {
        let th = phase(k, i, sup.base)?;
        let ws = sup.weight(i) * si;
        re += ws * th.cos();
        im -= ws * th.sin();
    }
    Ok((re, im))
}

fn waveform_grad(
    s: &[f64],
    sup: &WindowSupport,
    sample: usize,
    reference: &[f64],
    period_rows: Option<usize>,
    normalise: bool,
    beta: f64,
) -> CaeResult<ColumnGrad> {
    let range = sup.from..sup.to();
    let mut d = vec![0.0; s.len()];
    let p = waveform_period(period_rows, sup)?;
    let (r, v) = waveform_reference(reference, p, normalise)?;
    let rows: Vec<usize> = range.clone().collect();
    let w: Vec<f64> = rows.iter().map(|&i| sup.weight(i)).collect();
    let (mean, sigma) = if normalise {
        let m: f64 = rows.iter().zip(&w).map(|(&i, wi)| wi * s[i]).sum();
        let var: f64 = rows.iter().zip(&w).map(|(&i, wi)| wi * (s[i] - m) * (s[i] - m)).sum();
        let scale = rows.iter().map(|&i| s[i].abs()).fold(0.0, f64::max);
        if var <= (64.0 * f64::EPSILON * scale).powi(2) {
            return Err(CaeError::convergence(format!(
                "waveform_mismatch: sample column {sample} is constant over the window; its standardised \
                         shape is not defined"
            )));
        }
        (m, var.sqrt())
    } else {
        (0.0, 1.0)
    };
    let a: Vec<f64> = rows.iter().map(|&i| (s[i] - mean) / sigma).collect();
    let dist: Vec<f64> = (0..p)
        .map(|tau| {
            a.iter()
                .enumerate()
                .map(|(l, al)| {
                    let e = al - r[waveform_index(l, tau, p)];
                    w[l] * e * e
                })
                .sum::<f64>()
                / v
        })
        .collect();
    let dmin = dist.iter().copied().fold(f64::INFINITY, f64::min);
    let e: Vec<f64> = dist.iter().map(|d| (-beta * (d - dmin)).exp()).collect();
    let z: f64 = e.iter().sum();
    let weights: Vec<f64> = e.iter().map(|x| x / z).collect();
    let value: f64 = weights.iter().zip(&dist).map(|(q, d)| q * d).sum();
    let c: Vec<f64> = weights.iter().zip(&dist).map(|(q, d)| q * (1.0 - beta * (d - value))).collect();
    let csum: f64 = c.iter().sum();
    let g: Vec<f64> = a
        .iter()
        .enumerate()
        .map(|(l, al)| {
            let aligned: f64 = (0..p).map(|tau| c[tau] * r[waveform_index(l, tau, p)]).sum();
            2.0 * w[l] / v * (al * csum - aligned)
        })
        .collect();
    if normalise {
        let big_g: f64 = g.iter().sum();
        let big_h: f64 = g.iter().zip(&a).map(|(gl, al)| gl * al).sum();
        for (l, &i) in rows.iter().enumerate() {
            d[i] = (g[l] - w[l] * big_g - w[l] * a[l] * big_h) / sigma;
        }
    } else {
        for (l, &i) in rows.iter().enumerate() {
            d[i] = g[l];
        }
    }
    Ok(ColumnGrad { value, d_column: d, d_step: 0.0 })
}

#[allow(clippy::too_many_lines)]
fn column_grad(
    f: &SeriesFunctional,
    s: &[f64],
    sup: &WindowSupport,
    dt: f64,
    periodic: bool,
) -> CaeResult<ColumnGrad> {
    let n = s.len();
    let range = sup.from..sup.to();
    let mut d = vec![0.0; n];
    let plain = |value: f64, d_column: Vec<f64>| -> CaeResult<ColumnGrad> {
        Ok(ColumnGrad { value, d_column, d_step: 0.0 })
    };
    match f {
        SeriesFunctional::Mean { .. } => {
            let mut v = 0.0;
            for i in range {
                v += sup.weight(i) * s[i];
                d[i] = sup.weight(i);
            }
            plain(v, d)
        }
        SeriesFunctional::MeanSquare { .. } => {
            let mut v = 0.0;
            for i in range {
                v += sup.weight(i) * s[i] * s[i];
                d[i] = 2.0 * sup.weight(i) * s[i];
            }
            plain(v, d)
        }
        SeriesFunctional::Rms { sample } => {
            let ms: f64 = range.clone().map(|i| sup.weight(i) * s[i] * s[i]).sum();
            if ms <= 0.0 {
                return Err(CaeError::convergence(format!(
                    "rms of sample column {sample} vanishes; the root mean square is not differentiable there"
                )));
            }
            let v = ms.sqrt();
            for i in range {
                d[i] = sup.weight(i) * s[i] / v;
            }
            plain(v, d)
        }
        SeriesFunctional::Variance { .. } => {
            let mean: f64 = range.clone().map(|i| sup.weight(i) * s[i]).sum();
            let (mut v, mut first) = (0.0, 0.0);
            for i in range.clone() {
                let dev = s[i] - mean;
                v += sup.weight(i) * dev * dev;
                first += sup.weight(i) * dev;
            }
            for i in range {
                d[i] = 2.0 * sup.weight(i) * ((s[i] - mean) - first);
            }
            plain(v, d)
        }
        SeriesFunctional::SmoothMax { beta, .. } => {
            let (v, g) = smooth_max_grad(s, sup, *beta, 1.0)?;
            plain(v, g)
        }
        SeriesFunctional::SmoothMin { beta, .. } => {
            let (v, g) = smooth_max_grad(s, sup, *beta, -1.0)?;
            plain(-v, g.iter().map(|x| -x).collect())
        }
        SeriesFunctional::SmoothPeakToPeak { beta, .. } => {
            let (hi, g_hi) = smooth_max_grad(s, sup, *beta, 1.0)?;
            let (lo, g_lo) = smooth_max_grad(s, sup, *beta, -1.0)?;
            plain(hi + lo, g_hi.iter().zip(&g_lo).map(|(a, b)| a + b).collect())
        }
        SeriesFunctional::Harmonic { sample, order, part } => {
            check_harmonic_order(*order, sup.base, "harmonic")?;
            let (re, im) = harmonic_coefficient(s, sup, *order)?;
            let modulus = re.hypot(im);
            let v = match part {
                HarmonicPart::Real => 2.0 * re,
                HarmonicPart::Imag => 2.0 * im,
                HarmonicPart::Amplitude => {
                    let scale: f64 = range.clone().map(|i| (sup.weight(i) * s[i]).abs()).sum();
                    if modulus <= 64.0 * f64::EPSILON * scale {
                        return Err(CaeError::convergence(format!(
                            "harmonic {order} of sample column {sample} vanishes to rounding; its amplitude is not \
                             differentiable there"
                        )));
                    }
                    2.0 * modulus
                }
            };
            for i in range {
                let th = phase(*order, i, sup.base)?;
                let (c, sn) = (sup.weight(i) * th.cos(), -sup.weight(i) * th.sin());
                d[i] = match part {
                    HarmonicPart::Real => 2.0 * c,
                    HarmonicPart::Imag => 2.0 * sn,
                    HarmonicPart::Amplitude => 2.0 * (re * c + im * sn) / modulus,
                };
            }
            plain(v, d)
        }
        SeriesFunctional::HarmonicFit { sample, order, part } => {
            let (re, im, map) = harmonic_fit(s, sup, *order)?;
            let amplitude = re.hypot(im);
            let value = match part {
                HarmonicPart::Real => re,
                HarmonicPart::Imag => im,
                HarmonicPart::Amplitude => {
                    let scale: f64 = range.clone().map(|i| sup.weight(i)*(s[i]-s[sup.from]).abs()).sum();
                    if amplitude <= 64.*f64::EPSILON*scale { return Err(CaeError::convergence(format!("harmonic_fit {order} of sample {sample} vanishes; amplitude is not differentiable"))); }
                    amplitude
                }
            };
            for i in range { d[i] = match part { HarmonicPart::Real => map[i][0], HarmonicPart::Imag => map[i][1], HarmonicPart::Amplitude => (re*map[i][0]+im*map[i][1])/amplitude }; }
            plain(value, d)
        }
        SeriesFunctional::BandPower { lo, hi, .. } => {
            check_harmonic_order(*hi, sup.base, "band_power")?;
            let mut v = 0.0;
            for k in *lo..=*hi {
                let (re, im) = harmonic_coefficient(s, sup, k)?;
                v += re * re + im * im;
                for i in range.clone() {
                    let th = phase(k, i, sup.base)?;
                    d[i] += 2.0 * sup.weight(i) * (re * th.cos() - im * th.sin());
                }
            }
            plain(v, d)
        }
        SeriesFunctional::CrossingPeriod { sample, level } => {
            let found = crossings(s, sup, *level)?;
            if sup.cyclic {
                if found.is_empty() {
                    return Err(CaeError::convergence(format!(
                        "crossing_period: sample column {sample} has no upward crossing of {level} in the period"
                    )));
                }
                let k = found.len() as f64;
                return Ok(ColumnGrad { value: n as f64 * dt / k, d_column: d, d_step: n as f64 / k });
            }
            if found.len() < 2 {
                return Err(CaeError::convergence(format!(
                    "crossing_period: sample column {sample} has {} upward crossings of {level} in the window; at \
                     least two are needed",
                    found.len()
                )));
            }
            let position = |(i, j): (usize, usize)| {
                let gap = s[j] - s[i];
                let frac = (level - s[i]) / gap;
                let di = (level - s[j]) / (gap * gap);
                let dj = -(level - s[i]) / (gap * gap);
                ((i + 1) as f64 + frac, di, dj)
            };
            let spans = (found.len() - 1) as f64;
            let first = found[0];
            let last = found[found.len() - 1];
            let (x1, d1i, d1j) = position(first);
            let (xk, dki, dkj) = position(last);
            let c = dt / spans;
            d[first.0] -= c * d1i;
            d[first.1] -= c * d1j;
            d[last.0] += c * dki;
            d[last.1] += c * dkj;
            let value = c * (xk - x1);
            Ok(ColumnGrad { value, d_column: d, d_step: (xk - x1) / spans })
        }
        SeriesFunctional::DutyFraction { level, width, .. } => {
            let mut v = 0.0;
            for i in range {
                let (h, dh) = smooth_step((s[i] - level + width) / (2.0 * width));
                v += sup.weight(i) * h;
                d[i] = sup.weight(i) * dh / (2.0 * width);
            }
            plain(v, d)
        }
        SeriesFunctional::WaveformMismatch { sample, reference, period_rows, normalise, beta } => {
            waveform_grad(s, sup, *sample, reference, *period_rows, *normalise, *beta)
        }
        SeriesFunctional::Rate { inner } => {
            rate_rows_check(n)?;
            let derived = rate_series(s, dt, periodic);
            let g = column_grad(inner, &derived, sup, dt, periodic)?;
            let through_rate: f64 = g.d_column.iter().zip(&derived).map(|(a, b)| a * b).sum::<f64>() / dt;
            Ok(ColumnGrad {
                value: g.value,
                d_column: rate_transpose(&g.d_column, dt, periodic),
                d_step: g.d_step - through_rate,
            })
        }
    }
}

fn smooth_max_value<S: Scalar>(s: &[S], sup: &WindowSupport, beta: f64, sign: f64) -> CaeResult<S> {
    let shift = (sup.from..sup.to())
        .filter(|&i| sup.weight(i) > 0.0)
        .map(|i| sign * s[i].value())
        .fold(f64::NEG_INFINITY, f64::max);
    if !shift.is_finite() {
        return Err(CaeError::contract("smooth maximum over a window without positive weight"));
    }
    let total: S = (sup.from..sup.to()).map(|i| ((s[i] * sign - shift) * beta).exp() * sup.weight(i)).sum();
    Ok(total.ln() / beta + shift)
}

fn harmonic_value<S: Scalar>(s: &[S], sup: &WindowSupport, k: usize) -> CaeResult<(S, S)> {
    let (mut re, mut im) = (S::zero(), S::zero());
    for (i, &si) in s.iter().enumerate().take(sup.to()).skip(sup.from) {
        let th = phase(k, i, sup.base)?;
        let ws = si * sup.weight(i);
        re += ws * th.cos();
        im -= ws * th.sin();
    }
    Ok((re, im))
}

fn waveform_value<S: Scalar>(
    s: &[S],
    sup: &WindowSupport,
    sample: usize,
    reference: &[f64],
    period_rows: Option<usize>,
    normalise: bool,
    beta: f64,
) -> CaeResult<S> {
    let range = sup.from..sup.to();
    let p = waveform_period(period_rows, sup)?;
    let (r, v) = waveform_reference(reference, p, normalise)?;
    let rows: Vec<usize> = range.clone().collect();
    let a: Vec<S> = if normalise {
        let m: S = rows.iter().map(|&i| s[i] * sup.weight(i)).sum();
        let var: S = rows.iter().map(|&i| (s[i] - m) * (s[i] - m) * sup.weight(i)).sum();
        let scale = rows.iter().map(|&i| s[i].value().abs()).fold(0.0, f64::max);
        if var.value() <= (64.0 * f64::EPSILON * scale).powi(2) {
            return Err(CaeError::convergence(format!(
                "waveform_mismatch: sample column {sample} is constant over the window; its standardised \
                         shape is not defined"
            )));
        }
        let sigma = var.sqrt();
        rows.iter().map(|&i| (s[i] - m) / sigma).collect()
    } else {
        rows.iter().map(|&i| s[i]).collect()
    };
    let dist: Vec<S> = (0..p)
        .map(|tau| {
            a.iter()
                .enumerate()
                .map(|(l, al)| {
                    let e = *al - r[waveform_index(l, tau, p)];
                    e * e * sup.weight(rows[l])
                })
                .sum::<S>()
                / v
        })
        .collect();
    let dmin = dist.iter().map(Scalar::value).fold(f64::INFINITY, f64::min);
    let e: Vec<S> = dist.iter().map(|d| ((*d - dmin) * -beta).exp()).collect();
    let z: S = e.iter().copied().sum();
    Ok(e.iter().zip(&dist).map(|(x, d)| *x * *d).sum::<S>() / z)
}

fn column_value<S: Scalar>(
    f: &SeriesFunctional,
    s: &[S],
    sup: &WindowSupport,
    dt: S,
    periodic: bool,
) -> CaeResult<S> {
    let range = sup.from..sup.to();
    let weighted = |g: &dyn Fn(S) -> S| -> S { range.clone().map(|i| g(s[i]) * sup.weight(i)).sum() };
    match f {
        SeriesFunctional::Mean { .. } => Ok(weighted(&|x| x)),
        SeriesFunctional::MeanSquare { .. } => Ok(weighted(&|x| x * x)),
        SeriesFunctional::Rms { sample } => {
            let ms = weighted(&|x| x * x);
            if ms.value() <= 0.0 {
                return Err(CaeError::convergence(format!(
                    "rms of sample column {sample} vanishes; the root mean square is not differentiable there"
                )));
            }
            Ok(ms.sqrt())
        }
        SeriesFunctional::Variance { .. } => {
            let mean = weighted(&|x| x);
            Ok(weighted(&|x| (x - mean) * (x - mean)))
        }
        SeriesFunctional::SmoothMax { beta, .. } => smooth_max_value(s, sup, *beta, 1.0),
        SeriesFunctional::SmoothMin { beta, .. } => Ok(-smooth_max_value(s, sup, *beta, -1.0)?),
        SeriesFunctional::SmoothPeakToPeak { beta, .. } => {
            Ok(smooth_max_value(s, sup, *beta, 1.0)? + smooth_max_value(s, sup, *beta, -1.0)?)
        }
        SeriesFunctional::Harmonic { sample, order, part } => {
            check_harmonic_order(*order, sup.base, "harmonic")?;
            let (re, im) = harmonic_value(s, sup, *order)?;
            Ok(match part {
                HarmonicPart::Real => re * 2.0,
                HarmonicPart::Imag => im * 2.0,
                HarmonicPart::Amplitude => {
                    let modulus = (re * re + im * im).sqrt();
                    let scale: f64 = range.clone().map(|i| (sup.weight(i) * s[i].value()).abs()).sum();
                    if modulus.value() <= 64.0 * f64::EPSILON * scale {
                        return Err(CaeError::convergence(format!(
                            "harmonic {order} of sample column {sample} vanishes to rounding; its amplitude is not \
                             differentiable there"
                        )));
                    }
                    modulus * 2.0
                }
            })
        }
        SeriesFunctional::HarmonicFit { sample, order, part } => {
            let (re, im, _) = harmonic_fit(s, sup, *order)?;
            Ok(match part {
                HarmonicPart::Real => re,
                HarmonicPart::Imag => im,
                HarmonicPart::Amplitude => {
                    let amplitude = (re*re+im*im).sqrt();
                    let scale: f64 = range.clone().map(|i| sup.weight(i)*(s[i].value()-s[sup.from].value()).abs()).sum();
                    if amplitude.value() <= 64.*f64::EPSILON*scale { return Err(CaeError::convergence(format!("harmonic_fit {order} of sample {sample} vanishes; amplitude is not differentiable"))); }
                    amplitude
                }
            })
        }
        SeriesFunctional::BandPower { lo, hi, .. } => {
            check_harmonic_order(*hi, sup.base, "band_power")?;
            let mut total = S::zero();
            for k in *lo..=*hi {
                let (re, im) = harmonic_value(s, sup, k)?;
                total += re * re + im * im;
            }
            Ok(total)
        }
        SeriesFunctional::CrossingPeriod { sample, level } => {
            let values: Vec<f64> = s.iter().map(Scalar::value).collect();
            let found = crossings(&values, sup, *level)?;
            if sup.cyclic {
                if found.is_empty() {
                    return Err(CaeError::convergence(format!(
                        "crossing_period: sample column {sample} has no upward crossing of {level} in the period"
                    )));
                }
                return Ok(dt * (s.len() as f64 / found.len() as f64));
            }
            if found.len() < 2 {
                return Err(CaeError::convergence(format!(
                    "crossing_period: sample column {sample} has {} upward crossings of {level} in the window; at \
                     least two are needed",
                    found.len()
                )));
            }
            let position = |(i, j): (usize, usize)| (s[i] * -1.0 + *level) / (s[j] - s[i]) + (i + 1) as f64;
            let first = position(found[0]);
            let last = position(found[found.len() - 1]);
            Ok((last - first) * dt / (found.len() - 1) as f64)
        }
        SeriesFunctional::DutyFraction { level, width, .. } => {
            let step = |x: S| {
                let u = (x - *level + *width) / (2.0 * width);
                if u.value() <= 0.0 {
                    S::zero()
                } else if u.value() >= 1.0 {
                    S::one()
                } else {
                    u * u * u * (u * (u * 6.0 - 15.0) + 10.0)
                }
            };
            Ok(weighted(&step))
        }
        SeriesFunctional::WaveformMismatch { sample, reference, period_rows, normalise, beta } => {
            waveform_value(s, sup, *sample, reference, *period_rows, *normalise, *beta)
        }
        SeriesFunctional::Rate { inner } => {
            rate_rows_check(s.len())?;
            let derived = rate_series(s, dt, periodic);
            column_value(inner, &derived, sup, dt, periodic)
        }
    }
}

fn check_request(functional: &SeriesFunctional, columns: usize, step_s: f64) -> CaeResult<()> {
    functional.validate()?;
    if !(step_s.is_finite() && step_s > 0.0) {
        return Err(CaeError::contract("time functional step size must be finite and positive"));
    }
    if functional.sample() >= columns {
        return Err(CaeError::contract(format!(
            "{} reads sample column {} but the series has {columns} sample columns",
            functional.kind(),
            functional.sample()
        )));
    }
    Ok(())
}



pub fn evaluate(
    functional: &SeriesFunctional,
    window: &TimeWindow,
    samples: &DenseMatrix,
    step_s: f64,
    periodic: bool,
    autonomous: bool,
) -> CaeResult<FunctionalValue> {
    check_request(functional, samples.ncols, step_s)?;
    if samples.data.len() != samples.nrows * samples.ncols {
        return Err(CaeError::contract("sample matrix data does not match its shape"));
    }
    let support = window.support(samples.nrows, periodic, autonomous)?;
    let column_index = functional.sample();
    let column: Vec<f64> =
        (0..samples.nrows).map(|i| samples.data[i * samples.ncols + column_index]).collect();
    if let Some(i) = column.iter().position(|v| !v.is_finite()) {
        return Err(CaeError::convergence(format!("sample column {column_index} is not finite at row {i}")));
    }
    let g = column_grad(functional, &column, &support, step_s, periodic)?;
    if !g.value.is_finite() || g.d_column.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::convergence(format!(
            "{} of sample column {column_index} overflowed",
            functional.kind()
        )));
    }
    let mut d_samples = DenseMatrix::zeros(samples.nrows, samples.ncols);
    for (i, v) in g.d_column.iter().enumerate() {
        d_samples.data[i * samples.ncols + column_index] = *v;
    }
    let d_period_s = if periodic && autonomous { g.d_step / samples.nrows as f64 } else { 0.0 };
    Ok(FunctionalValue { value: g.value, d_samples, d_period_s })
}


pub fn evaluate_value<S: Scalar>(
    functional: &SeriesFunctional,
    window: &TimeWindow,
    samples: &[S],
    columns: usize,
    step_s: S,
    periodic: bool,
    autonomous: bool,
) -> CaeResult<S> {
    check_request(functional, columns, step_s.value())?;
    if !samples.len().is_multiple_of(columns) {
        return Err(CaeError::contract("sample matrix data does not match its shape"));
    }
    let rows = samples.len() / columns;
    let support = window.support(rows, periodic, autonomous)?;
    let column_index = functional.sample();
    let column: Vec<S> = (0..rows).map(|i| samples[i * columns + column_index]).collect();
    if let Some(i) = column.iter().position(|v| !v.is_finite()) {
        return Err(CaeError::convergence(format!("sample column {column_index} is not finite at row {i}")));
    }
    let step = if periodic && autonomous { step_s } else { S::from_f64(step_s.value()) };
    column_value(functional, &column, &support, step, periodic)
}
