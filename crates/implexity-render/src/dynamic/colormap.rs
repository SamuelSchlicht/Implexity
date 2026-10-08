// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::LazyLock;

use super::colormap_data::{INFERNO, MAGMA, PLASMA, VIRIDIS};
use crate::RenderError;

pub const NAN_COLOUR: [u8; 3] = [128, 128, 128];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colormap {
    Viridis,
    Magma,
    Inferno,
    Plasma,
    CoolWarm,
    Phase,
    Gray,
}

pub const NAMES: [&str; 7] = ["viridis", "magma", "inferno", "plasma", "coolwarm", "phase", "gray"];

impl Colormap {


    pub fn parse(name: &str) -> Result<Self, RenderError> {
        Ok(match name {
            "viridis" => Self::Viridis,
            "magma" => Self::Magma,
            "inferno" => Self::Inferno,
            "plasma" => Self::Plasma,
            "coolwarm" => Self::CoolWarm,
            "phase" => Self::Phase,
            "gray" => Self::Gray,
            other => {
                return Err(RenderError::Invalid(format!(
                    "colormap must be one of {}, not {other:?}",
                    NAMES.join(", ")
                )));
            }
        })
    }

    #[must_use]
    pub fn for_hint(hint: &str) -> Self {
        match hint {
            "diverging" => Self::CoolWarm,
            "cyclic" => Self::Phase,
            _ => Self::Viridis,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Viridis => "viridis",
            Self::Magma => "magma",
            Self::Inferno => "inferno",
            Self::Plasma => "plasma",
            Self::CoolWarm => "coolwarm",
            Self::Phase => "phase",
            Self::Gray => "gray",
        }
    }

    #[must_use]
    pub const fn is_cyclic(self) -> bool {
        matches!(self, Self::Phase)
    }

    #[must_use]
    pub fn lut(self) -> &'static [[u8; 3]; 256] {
        match self {
            Self::Viridis => &VIRIDIS,
            Self::Magma => &MAGMA,
            Self::Inferno => &INFERNO,
            Self::Plasma => &PLASMA,
            Self::CoolWarm => &COOLWARM,
            Self::Phase => &PHASE,
            Self::Gray => &GRAY,
        }
    }

    #[must_use]
    pub fn map(self, t: f64) -> [u8; 3] {
        if !t.is_finite() {
            return NAN_COLOUR;
        }
        let u = if self.is_cyclic() { t - t.floor() } else { t.clamp(0.0, 1.0) };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i = ((u * 255.0).round() as usize).min(255);
        self.lut()[i]
    }
}

const WHITE: [f64; 3] = [0.950_47, 1.0, 1.088_83];

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn f_lab(t: f64) -> f64 {
    let d: f64 = 6.0 / 29.0;
    if t > d * d * d { t.cbrt() } else { t / (3.0 * d * d) + 4.0 / 29.0 }
}

fn f_lab_inv(t: f64) -> f64 {
    let d: f64 = 6.0 / 29.0;
    if t > d { t * t * t } else { 3.0 * d * d * (t - 4.0 / 29.0) }
}

#[must_use]
pub fn rgb_to_lab(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|c| srgb_to_linear(c / 255.0));
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
    let (fx, fy, fz) = (f_lab(x / WHITE[0]), f_lab(y / WHITE[1]), f_lab(z / WHITE[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

#[must_use]
pub fn lab_to_rgb(lab: [f64; 3]) -> [f64; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let (fx, fz) = (fy + lab[1] / 500.0, fy - lab[2] / 200.0);
    let (x, y, z) = (WHITE[0] * f_lab_inv(fx), WHITE[1] * f_lab_inv(fy), WHITE[2] * f_lab_inv(fz));
    let r = 3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z;
    let g = -0.969_266_0 * x + 1.876_010_8 * y + 0.041_556_0 * z;
    let b = 0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z;
    [r, g, b].map(|c| 255.0 * linear_to_srgb(c.clamp(0.0, 1.0)))
}

fn to_msh(lab: [f64; 3]) -> [f64; 3] {
    let m = (lab[0] * lab[0] + lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    let s = if m > 0.0 { (lab[0] / m).clamp(-1.0, 1.0).acos() } else { 0.0 };
    let h = lab[2].atan2(lab[1]);
    [m, s, h]
}

fn from_msh(msh: [f64; 3]) -> [f64; 3] {
    let [m, s, h] = msh;
    [m * s.cos(), m * s.sin() * h.cos(), m * s.sin() * h.sin()]
}

fn adjust_hue(msh: [f64; 3], m_unsat: f64) -> f64 {
    let [m, s, h] = msh;
    if m >= m_unsat {
        return h;
    }
    let spin = s * (m_unsat * m_unsat - m * m).sqrt() / (m * s.sin());
    if h > -std::f64::consts::FRAC_PI_3 { h + spin } else { h - spin }
}

#[must_use]
pub fn diverging(rgb1: [f64; 3], rgb2: [f64; 3], t: f64) -> [f64; 3] {
    let (mut a, mut b) = (to_msh(rgb_to_lab(rgb1)), to_msh(rgb_to_lab(rgb2)));
    let mut t = t;
    if a[1] > 0.05 && b[1] > 0.05 && angle_diff(a[2], b[2]) > std::f64::consts::FRAC_PI_3 {
        let mid = a[0].max(b[0]).max(88.0);
        if t < 0.5 {
            b = [mid, 0.0, 0.0];
            t *= 2.0;
        } else {
            a = [mid, 0.0, 0.0];
            t = 2.0 * t - 1.0;
        }
    }
    if a[1] < 0.05 && b[1] > 0.05 {
        a[2] = adjust_hue(b, a[0]);
    } else if b[1] < 0.05 && a[1] > 0.05 {
        b[2] = adjust_hue(a, b[0]);
    }
    let msh = [0, 1, 2].map(|i| (1.0 - t) * a[i] + t * b[i]);
    lab_to_rgb(from_msh(msh))
}

fn angle_diff(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % (2.0 * std::f64::consts::PI);
    d.min(2.0 * std::f64::consts::PI - d)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn quantise(c: [f64; 3]) -> [u8; 3] {
    c.map(|v| v.round().clamp(0.0, 255.0) as u8)
}

static COOLWARM: LazyLock<[[u8; 3]; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| quantise(diverging([59.0, 76.0, 192.0], [180.0, 4.0, 38.0], i as f64 / 255.0)))
});

fn phase_path(theta: f64) -> [f64; 3] {
    [62.0 + 24.0 * theta.cos(), 8.0 * (2.0 * theta).sin(), 42.0 * theta.sin()]
}

static PHASE: LazyLock<[[u8; 3]; 256]> = LazyLock::new(|| {
    let fine = 4096;
    let pts: Vec<[f64; 3]> =
        (0..=fine).map(|k| phase_path(2.0 * std::f64::consts::PI * k as f64 / fine as f64)).collect();
    let mut cum = vec![0.0; fine + 1];
    for k in 1..=fine {
        let d: f64 = (0..3).map(|i| (pts[k][i] - pts[k - 1][i]).powi(2)).sum::<f64>().sqrt();
        cum[k] = cum[k - 1] + d;
    }
    let total = cum[fine];
    std::array::from_fn(|i| {
        let target = total * i as f64 / 256.0;
        let k = cum.partition_point(|c| *c < target).clamp(1, fine);
        let s = (target - cum[k - 1]) / (cum[k] - cum[k - 1]).max(1e-300);
        let lab = [0, 1, 2].map(|c| pts[k - 1][c] + s * (pts[k][c] - pts[k - 1][c]));
        quantise(lab_to_rgb(lab))
    })
});

static GRAY: LazyLock<[[u8; 3]; 256]> = LazyLock::new(|| {
    #[allow(clippy::cast_possible_truncation)]
    std::array::from_fn(|i| [i as u8; 3])
});

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    pub map: Colormap,
    pub lo: f64,
    pub hi: f64,
}

impl Scale {
    #[must_use]
    pub fn new(map: Colormap, lo: f64, hi: f64) -> Self {
        let (lo, hi) = if lo.is_finite() && hi.is_finite() { (lo.min(hi), lo.max(hi)) } else { (0.0, 1.0) };
        if hi - lo <= 1e-300_f64.max(1e-12 * hi.abs().max(lo.abs())) {
            let w = if lo == 0.0 { 1.0 } else { 0.5 * lo.abs() };
            return Self { map, lo: lo - w, hi: hi + w };
        }
        Self { map, lo, hi }
    }

    #[must_use]
    pub fn symmetric(map: Colormap, lo: f64, hi: f64) -> Self {
        let m = lo.abs().max(hi.abs());
        Self::new(map, -m, m)
    }

    #[must_use]
    pub fn colour(&self, v: f64) -> [u8; 3] {
        self.map.map((v - self.lo) / (self.hi - self.lo))
    }
}

