// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

mod font;
mod png;
mod textwrap;

pub use font::{lower_glyph, upper_glyph};
pub use png::{read_png, write_png, write_png_rgba};
pub use textwrap::wrap as text_wrap;

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::MeshError;
use crate::numeric::{linspace, searchsorted, to_u8};
use crate::pyfmt::{fmt_upper_f, fmt_upper_g};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbImage {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl RgbImage {
    #[must_use]
    pub fn filled(width: usize, height: usize, colour: [u8; 3]) -> Self {
        let mut data = Vec::with_capacity(width * height * 3);
        for _ in 0..width * height {
            data.extend_from_slice(&colour);
        }
        Self { width, height, data }
    }

    #[must_use]
    pub fn get(&self, row: usize, col: usize) -> [u8; 3] {
        let i = (row * self.width + col) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    pub fn set(&mut self, row: usize, col: usize, colour: [u8; 3]) {
        let i = (row * self.width + col) * 3;
        self.data[i..i + 3].copy_from_slice(&colour);
    }

    pub fn fill_rect(&mut self, rows: std::ops::Range<usize>, cols: std::ops::Range<usize>, colour: [u8; 3]) {
        for r in rows {
            for c in cols.clone() {
                self.set(r, c, colour);
            }
        }
    }

    #[must_use]
    pub fn to_png(&self) -> Vec<u8> {
        write_png(&self.data, self.width, self.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Background {
    Dark,
    White,
}

impl Background {


    pub fn parse(s: &str) -> Result<Self, MeshError> {
        match s {
            "dark" => Ok(Self::Dark),
            "white" => Ok(Self::White),
            _ => Err(MeshError::invalid("section background must be dark or white")),
        }
    }
}

const VOID: [f64; 3] = [18.0, 20.0, 24.0];
const PHASE_LOW: [f64; 3] = [196.0, 200.0, 208.0];
const PHASE_HIGH: [f64; 3] = [206.0, 122.0, 62.0];
pub const OUTSIDE: [f64; 3] = [8.0, 9.0, 11.0];
pub const WHITE_BACKGROUND: [f64; 3] = [255.0, 255.0, 255.0];
const WHITE_SOLID: [f64; 3] = [164.0, 172.0, 184.0];
const WHITE_PHASE_HIGH: [f64; 3] = [184.0, 92.0, 38.0];

pub const SEQUENTIAL: [[f64; 3]; 4] =
    [[10.0, 25.0, 44.0], [20.0, 102.0, 133.0], [45.0, 196.0, 187.0], [244.0, 222.0, 133.0]];
pub const DIVERGING: [[f64; 3]; 5] = [
    [35.0, 91.0, 168.0],
    [143.0, 191.0, 219.0],
    [235.0, 237.0, 232.0],
    [236.0, 157.0, 83.0],
    [180.0, 63.0, 50.0],
];
const REGION_COLOURS: [[u8; 3]; 6] =
    [[0, 225, 255], [255, 184, 75], [236, 91, 137], [146, 231, 112], [176, 134, 255], [255, 237, 92]];
const REGION_COLOURS_WHITE: [[u8; 3]; 6] =
    [[0, 105, 150], [186, 88, 0], [184, 45, 92], [45, 126, 28], [104, 65, 176], [140, 106, 0]];

fn u8c(c: [f64; 3]) -> [u8; 3] {
    [to_u8(c[0]), to_u8(c[1]), to_u8(c[2])]
}

fn py_range(start: i64, stop: i64, len: usize) -> std::ops::Range<usize> {
    let n = i64::try_from(len).unwrap_or(i64::MAX);
    let norm = |v: i64| if v < 0 { (v + n).max(0) } else { v.min(n) };
    let (a, b) = (norm(start), norm(stop));
    if b <= a { 0..0 } else { usize::try_from(a).unwrap_or(0)..usize::try_from(b).unwrap_or(0) }
}

pub fn draw_text(img: &mut RgbImage, x: i64, y: i64, value: &str, colour: [u8; 3], scale: i64) {
    let (h, w) = (i64::try_from(img.height).unwrap_or(0), i64::try_from(img.width).unwrap_or(0));
    let mut cursor = x;
    for ch in value.to_uppercase().chars() {
        let glyph = upper_glyph(ch).unwrap_or(["00000"; 7]);
        for (gy, row) in glyph.iter().enumerate() {
            for (gx, bit) in row.chars().enumerate() {
                if bit != '1' {
                    continue;
                }
                let x0 = cursor + gx as i64 * scale;
                let y0 = y + gy as i64 * scale;
                if x0 < w && y0 < h {
                    let rows = py_range(y0.max(0), (y0 + scale).min(h), img.height);
                    let cols = py_range(x0.max(0), (x0 + scale).min(w), img.width);
                    img.fill_rect(rows, cols, colour);
                }
            }
        }
        cursor += 6 * scale;
        if cursor >= w - 5 * scale {
            break;
        }
    }
}

pub fn draw_text_chip(
    img: &mut RgbImage,
    x: i64,
    y: i64,
    value: &str,
    colour: [u8; 3],
    background: [u8; 3],
    max_chars: Option<usize>,
) {
    let text: String = match max_chars {
        Some(n) => value.chars().take(n).collect(),
        None => value.to_string(),
    };
    let (h, w) = (i64::try_from(img.height).unwrap_or(0), i64::try_from(img.width).unwrap_or(0));
    let len = i64::try_from(text.chars().count()).unwrap_or(0);
    let (left, top) = ((x - 2).max(0), (y - 2).max(0));
    let right = w.min(x + 6 * len + 2);
    let bottom = h.min(y + 9);
    if right > left && bottom > top {
        let rows = py_range(top, bottom, img.height);
        let cols = py_range(left, right, img.width);
        img.fill_rect(rows, cols, background);
    }
    draw_text(img, x, y, &text, colour, 1);
}

const CHIP_TEXT: [u8; 3] = [245, 247, 249];
const CHIP_BACKGROUND: [u8; 3] = [10, 16, 25];

#[must_use]
pub fn ramp(value: f64, colours: &[[f64; 3]]) -> [f64; 3] {
    let v = if value.is_nan() { 0.0 } else { value.clamp(0.0, 1.0) };
    let scaled = v * (colours.len() - 1) as f64;
    let lo = scaled.floor();
    let lo_i = lo as usize;
    let hi_i = (lo_i + 1).min(colours.len() - 1);
    let t = scaled - lo;
    let (a, b) = (colours[lo_i], colours[hi_i]);
    [a[0] * (1.0 - t) + b[0] * t, a[1] * (1.0 - t) + b[1] * t, a[2] * (1.0 - t) + b[2] * t]
}

#[derive(Clone, Debug, PartialEq)]
pub struct CategoricalColours {
    pub thresholds: Vec<f64>,
    pub colours: Vec<[f64; 3]>,
}

impl CategoricalColours {


    pub fn from_json(spec: Option<&Value>) -> Result<Self, MeshError> {
        let Some(spec) = spec.and_then(Value::as_object) else {
            return Err(MeshError::invalid("categorical rendering requires a palette specification"));
        };
        let disagree = || MeshError::invalid("categorical palette thresholds and categories disagree");
        let thresholds: Vec<f64> = match spec.get("thresholds") {
            Some(Value::Array(a)) => {
                a.iter().map(|v| v.as_f64().ok_or_else(disagree)).collect::<Result<_, _>>()?
            }
            _ => return Err(disagree()),
        };
        let Some(Value::Array(categories)) = spec.get("categories") else {
            return Err(disagree());
        };
        if thresholds.iter().any(|t| !t.is_finite())
            || thresholds.windows(2).any(|p| p[1] - p[0] <= 0.0)
            || categories.len() != thresholds.len() + 1
        {
            return Err(disagree());
        }
        let require = || MeshError::invalid("categorical palette categories require RGB colors");
        let mut rows: Vec<Vec<f64>> = Vec::new();
        for row in categories {
            let rgb = row.get("color_rgb").and_then(Value::as_array).ok_or_else(require)?;
            rows.push(rgb.iter().map(|v| v.as_f64().ok_or_else(require)).collect::<Result<_, _>>()?);
        }
        if rows.windows(2).any(|p| p[0].len() != p[1].len()) {
            return Err(require());
        }
        let range = || MeshError::invalid("categorical palette RGB colors must lie in [0, 255]");
        let mut colours = Vec::with_capacity(rows.len());
        for r in rows {
            if r.len() != 3 || r.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 255.0) {
                return Err(range());
            }
            colours.push([r[0], r[1], r[2]]);
        }
        Ok(Self { thresholds, colours })
    }

    #[must_use]
    pub fn colour(&self, v: f64) -> [f64; 3] {
        self.colours[searchsorted(&self.thresholds, v, true).min(self.colours.len() - 1)]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SectionPalette {
    Sequential,
    Diverging,
    Categorical(CategoricalColours),
}

impl SectionPalette {


    pub fn from_name(name: &str, categorical: Option<&Value>) -> Result<Self, MeshError> {
        Ok(match name {
            "diverging" => Self::Diverging,
            "categorical" => Self::Categorical(CategoricalColours::from_json(categorical)?),
            _ => Self::Sequential,
        })
    }

    fn colours(&self) -> &'static [[f64; 3]] {
        match self {
            Self::Diverging => &DIVERGING,
            _ => &SEQUENTIAL,
        }
    }

    fn colour(&self, v: f64, lower: f64, upper: f64) -> [f64; 3] {
        match self {
            Self::Categorical(c) => c.colour(v),
            _ => ramp((v - lower) / (upper - lower), self.colours()),
        }
    }

    fn bar_colour(&self, t: f64, lower: f64, upper: f64) -> [f64; 3] {
        match self {
            Self::Categorical(c) => c.colour(lower + (upper - lower) * t),
            _ => ramp(t, self.colours()),
        }
    }
}



pub fn scalar_section_value_range(
    values: &[f64],
    value_range: Option<[f64; 2]>,
) -> Result<(f64, f64), MeshError> {
    let (mut lower, mut upper) = if let Some([a, b]) = value_range {
        (a, b)
    } else {
        {
            if !values.iter().any(|v| v.is_finite()) {
                return Err(MeshError::invalid("scalar section contains no finite values"));
            }
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for &v in values.iter().filter(|v| !v.is_nan()) {
                lo = lo.min(v);
                hi = hi.max(v);
            }
            (lo, hi)
        }
    };
    if !(lower + upper).is_finite() || upper <= lower {
        let pad = 1e-12_f64.max(lower.abs() * 1e-6);
        lower -= pad;
        upper += pad;
    }
    Ok((lower, upper))
}

#[derive(Clone, Debug)]
pub struct ScalarSectionOptions<'a> {
    pub outside: Option<&'a [bool]>,
    pub palette: SectionPalette,
    pub value_range: Option<[f64; 2]>,
    pub label: String,
    pub region_masks: Vec<&'a [bool]>,
    pub background: Background,
    pub draw_legend: bool,
}

impl Default for ScalarSectionOptions<'_> {
    fn default() -> Self {
        Self {
            outside: None,
            palette: SectionPalette::Sequential,
            value_range: None,
            label: "FIELD".to_string(),
            region_masks: Vec::new(),
            background: Background::Dark,
            draw_legend: true,
        }
    }
}

fn to_image(width: usize, height: usize, rgb: impl Fn(usize, usize) -> [f64; 3]) -> RgbImage {
    let mut img = RgbImage::filled(width, height, [0, 0, 0]);
    for row in 0..height {
        let y = height - 1 - row;
        for x in 0..width {
            img.set(row, x, u8c(rgb(x, y)));
        }
    }
    img
}



pub fn scalar_section_rgb(
    values: &[f64],
    width: usize,
    height: usize,
    opts: &ScalarSectionOptions<'_>,
) -> Result<RgbImage, MeshError> {
    let (lower, upper) = scalar_section_value_range(values, opts.value_range)?;
    let fill = match opts.background {
        Background::White => WHITE_BACKGROUND,
        Background::Dark => OUTSIDE,
    };
    let mut img = to_image(width, height, |x, y| {
        let v = values[x * height + y];
        let invalid = !v.is_finite() || opts.outside.is_some_and(|o| o[x * height + y]);
        if invalid { fill } else { opts.palette.colour(v, lower, upper) }
    });
    overlay_region_contours(&mut img, &opts.region_masks, opts.background);
    if !opts.draw_legend {
        return Ok(img);
    }
    let (h, w) = (i64::try_from(height).unwrap_or(0), i64::try_from(width).unwrap_or(0));
    let bar_h = 10.min(5.max(h / 35));
    let y1 = h - 4;
    let y0 = y1 - bar_h;
    let (x0, x1) = (8i64, 9.max(w - 8));
    let n = usize::try_from(1.max(x1 - x0)).unwrap_or(1);
    let ramp_t = linspace(0.0, 1.0, n);
    let rows = py_range(y0, y1, height);
    let cols = py_range(x0, x1, width);
    for r in rows {
        for (k, c) in cols.clone().enumerate() {
            if k < ramp_t.len() {
                img.set(r, c, u8c(opts.palette.bar_colour(ramp_t[k], lower, upper)));
            }
        }
    }
    let (text, bg) = match opts.background {
        Background::White => ([18, 28, 40], [255, 255, 255]),
        Background::Dark => (CHIP_TEXT, CHIP_BACKGROUND),
    };
    draw_text_chip(&mut img, 8, 0.max(y0 - 10), &opts.label, text, bg, Some(34));
    let low = fmt_upper_g(lower, 3);
    draw_text_chip(&mut img, 8, 0.max(y1 - 8), &low, text, bg, None);
    let high = fmt_upper_g(upper, 3);
    let hlen = i64::try_from(high.chars().count()).unwrap_or(0);
    draw_text_chip(&mut img, 8.max(w - 6 * hlen - 8), 0.max(y1 - 8), &high, text, bg, None);
    Ok(img)
}

pub fn overlay_region_contours(img: &mut RgbImage, region_masks: &[&[bool]], background: Background) {
    let colours = match background {
        Background::White => &REGION_COLOURS_WHITE,
        Background::Dark => &REGION_COLOURS,
    };
    let (h, w) = (img.height, img.width);
    for (index, mask) in region_masks.iter().enumerate() {
        let m = |r: usize, c: usize| mask[c * h + (h - 1 - r)];
        let colour = colours[index % colours.len()];
        for r in 0..h {
            for c in 0..w {
                if !m(r, c) {
                    continue;
                }
                let interior =
                    m((r + h - 1) % h, c) && m((r + 1) % h, c) && m(r, (c + w - 1) % w) && m(r, (c + 1) % w);
                let border = r == 0 || r == h - 1 || c == 0 || c == w - 1;
                if !interior || border {
                    img.set(r, c, colour);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhasePalette {
    colours: [[f64; 3]; 2],
}

impl PhasePalette {
    pub fn new(colours: [[f64; 3]; 2]) -> Result<Self, MeshError> {
        if colours.iter().flatten().any(|v| !v.is_finite() || !(0.0..=255.0).contains(v)) {
            return Err(MeshError::invalid("phase colours must be finite RGB values from 0 to 255"));
        }
        Ok(Self { colours })
    }

    #[must_use]
    pub fn for_background(background: Background) -> Self {
        Self { colours: match background {
            Background::White => [WHITE_SOLID, WHITE_PHASE_HIGH],
            Background::Dark => [PHASE_LOW, PHASE_HIGH],
        } }
    }
}

#[must_use]
pub fn section_rgb(
    rho: &[f64],
    width: usize,
    height: usize,
    phase_fraction: Option<&[f64]>,
    edge: Option<&[f64]>,
    outside: Option<&[bool]>,
    background: Background,
) -> RgbImage {
    section_rgb_with_palette(rho, width, height, phase_fraction, edge, outside, background, PhasePalette::for_background(background))
}

#[must_use]
pub fn section_rgb_with_palette(
    rho: &[f64],
    width: usize,
    height: usize,
    phase_fraction: Option<&[f64]>,
    edge: Option<&[f64]>,
    outside: Option<&[bool]>,
    background: Background,
    palette: PhasePalette,
) -> RgbImage {
    let (void, outside_colour) = match background {
        Background::White => (WHITE_BACKGROUND, WHITE_BACKGROUND),
        Background::Dark => (VOID, OUTSIDE),
    };
    let [low, high] = palette.colours;
    to_image(width, height, |x, y| {
        let i = x * height + y;
        if outside.is_some_and(|o| o[i]) {
            return outside_colour;
        }
        let r = crate::numeric::clip(rho[i], 0.0, 1.0);
        let solid = match phase_fraction {
            None => low,
            Some(phase_fraction) => {
                let c = crate::numeric::clip(phase_fraction[i], 0.0, 1.0);
                [
                    low[0] * (1.0 - c) + high[0] * c,
                    low[1] * (1.0 - c) + high[1] * c,
                    low[2] * (1.0 - c) + high[2] * c,
                ]
            }
        };
        let mut px = [0.0; 3];
        for k in 0..3 {
            px[k] = void[k] * (1.0 - r) + solid[k] * r;
        }
        if let Some(e) = edge {
            let f = 1.0 - 0.85 * crate::numeric::clip(e[i], 0.0, 1.0);
            for p in &mut px {
                *p *= f;
            }
        }
        px
    })
}

pub type RenderedSeries = (String, [u8; 3], f64, f64);

#[derive(Clone, Debug, PartialEq)]
pub struct ChartEvent {
    pub iteration: i64,
    pub label: String,
}


#[allow(clippy::too_many_lines)]
#[must_use]
pub fn history_chart_rgb(
    x_values: &[f64],
    series: &[(String, Vec<f64>)],
    events: &[ChartEvent],
    width: usize,
    height: usize,
    title: &str,
    background: Background,
) -> (RgbImage, Vec<RenderedSeries>, Vec<ChartEvent>) {
    let white = background == Background::White;
    let mut img = RgbImage::filled(width, height, if white { [255, 255, 255] } else { [10, 16, 25] });
    let (wi, hi_) = (i64::try_from(width).unwrap_or(0), i64::try_from(height).unwrap_or(0));
    let (left, right, top, bottom) = (54i64, wi - 16, 28i64, hi_ - 43);
    let grid = if white { [218, 224, 231] } else { [42, 54, 69] };
    let rows = |a: i64, b: i64| py_range(a, b, height);
    let cols = |a: i64, b: i64| py_range(a, b, width);
    for i in 0..6 {
        let y = top + ((bottom - top) as f64 * f64::from(i) / 5.0).round_ties_even() as i64;
        img.fill_rect(rows(y, y + 1), cols(left, right), grid);
    }
    for i in 0..7 {
        let x = left + ((right - left) as f64 * f64::from(i) / 6.0).round_ties_even() as i64;
        img.fill_rect(rows(top, bottom), cols(x, x + 1), grid);
    }
    let axis_colour = if white { [73, 84, 98] } else { [164, 178, 193] };
    let title_colour = if white { [20, 28, 40] } else { [237, 242, 247] };
    let axis_text = if white { [20, 28, 40] } else { [226, 233, 240] };
    img.fill_rect(rows(top, bottom), cols(left, left + 1), axis_colour);
    img.fill_rect(rows(bottom, bottom + 1), cols(left, right), axis_colour);
    let title70: String = title.chars().take(70).collect();
    draw_text(&mut img, left, 7, &title70, title_colour, 1);
    let (xlo, xhi) = if x_values.is_empty() {
        (0.0, 1.0)
    } else {
        let lo = x_values.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = x_values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (lo, hi.max(lo + 1.0))
    };
    let palette: [[u8; 3]; 8] = if white {
        [
            [0, 124, 116],
            [205, 92, 0],
            [39, 103, 190],
            [184, 45, 96],
            [104, 65, 176],
            [48, 130, 35],
            [158, 112, 0],
            [0, 116, 166],
        ]
    } else {
        [
            [53, 210, 196],
            [255, 176, 66],
            [104, 164, 255],
            [238, 96, 140],
            [181, 139, 255],
            [155, 220, 96],
            [255, 224, 105],
            [91, 200, 255],
        ]
    };
    let clip_i = |v: i64, lo: i64, hi: i64| v.max(lo).min(hi);
    let mut rendered = Vec::new();
    for (index, (name, raw)) in series.iter().enumerate() {
        let valid: Vec<usize> = (0..raw.len().min(x_values.len()))
            .filter(|&i| raw[i].is_finite() && x_values[i].is_finite())
            .collect();
        if valid.is_empty() {
            continue;
        }
        let yy: Vec<f64> = valid.iter().map(|&i| raw[i]).collect();
        let mut lo = yy.iter().copied().fold(f64::INFINITY, f64::min);
        let mut hi = yy.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if hi <= lo {
            lo -= 0.5;
            hi += 0.5;
        }
        let px: Vec<i64> = valid
            .iter()
            .map(|&i| {
                left + ((x_values[i] - xlo) / (xhi - xlo) * (right - left - 1) as f64).round_ties_even()
                    as i64
            })
            .collect();
        let py: Vec<i64> = yy
            .iter()
            .map(|&y| {
                bottom - 1 - ((y - lo) / (hi - lo) * (bottom - top - 1) as f64).round_ties_even() as i64
            })
            .collect();
        let colour = palette[index % palette.len()];
        let mut plot = |x: i64, y: i64| {
            let r = clip_i(y, top, bottom - 1);
            let c = clip_i(x, left, right - 1);
            if r >= 0 && c >= 0 && (r as usize) < height && (c as usize) < width {
                img.set(r as usize, c as usize, colour);
            }
        };
        for k in 1..px.len() {
            let (a, b, c, d) = (px[k - 1], py[k - 1], px[k], py[k]);
            let steps = (c - a).abs().max((d - b).abs()).max(1);
            let n = usize::try_from(steps + 1).unwrap_or(2);
            let xs = linspace(a as f64, c as f64, n);
            let zs = linspace(b as f64, d as f64, n);
            for (x, z) in xs.iter().zip(&zs) {
                plot(x.round_ties_even() as i64, z.round_ties_even() as i64);
            }
        }
        for (x, y) in px.iter().zip(&py) {
            plot(*x, *y);
        }
        rendered.push((name.clone(), colour, lo, hi));
        let legend_y = hi_ - 34 + (index as i64 % 2) * 12;
        let legend_x = left + (index as i64 / 2) * 145;
        if legend_x < right - 60 {
            img.fill_rect(rows(legend_y, legend_y + 5), cols(legend_x, legend_x + 12), colour);
            let short: String = name.chars().take(19).collect();
            draw_text(&mut img, legend_x + 17, legend_y - 1, &short, colour, 1);
        }
    }
    let mut markers = Vec::new();
    for event in events {
        let px = (left as f64 + (event.iteration as f64 - xlo) / (xhi - xlo) * (right - left - 1) as f64)
            .round_ties_even() as i64;
        if left <= px && px < right {
            let marker = if white { [50, 58, 70] } else { [245, 245, 245] };
            img.fill_rect(rows(top, bottom), cols(px, px + 1), marker);
            let short: String = event.label.chars().take(11).collect();
            draw_text(&mut img, (px + 3).min(right - 70), top + 3, &short, marker, 1);
            markers.push(event.clone());
        }
    }
    draw_text(&mut img, 5, top - 2, "NORM", axis_text, 1);
    draw_text(&mut img, left, bottom + 5, &fmt_upper_f(xlo, 0), axis_text, 1);
    let hi_text = fmt_upper_f(xhi, 0);
    let hlen = i64::try_from(hi_text.chars().count()).unwrap_or(0);
    draw_text(&mut img, right - 6 * hlen, bottom + 5, &hi_text, axis_text, 1);
    (img, rendered, markers)
}

#[derive(Clone, Debug)]
pub struct Annotation<'a> {
    pub units: Option<&'a str>,
    pub field_id: &'a str,
    pub plane: &'a str,
    pub at_mm: f64,
    pub bbox_mm: [[f64; 3]; 2],
    pub time_s: Option<f64>,
    pub sampled_bounds_mm: Option<[[f64; 2]; 2]>,
    pub label: Option<&'a str>,
}



#[allow(clippy::too_many_lines)]
pub fn annotated_scalar_section_rgb(
    values: &[f64],
    width: usize,
    height: usize,
    opts: &ScalarSectionOptions<'_>,
    ann: &Annotation<'_>,
) -> Result<(RgbImage, Value), MeshError> {
    fn put(image: &mut RgbImage, x: i64, y: i64, value: &str, colour: [u8; 3]) {
        for (i, c) in value.chars().enumerate() {
            let glyph = lower_glyph(c).or_else(|| upper_glyph(c)).unwrap_or(["00000"; 7]);
            for (gy, row) in glyph.iter().enumerate() {
                for (gx, bit) in row.chars().enumerate() {
                    let xx = x + 6 * i as i64 + gx as i64;
                    let yy = y + gy as i64;
                    if bit == '1'
                        && xx >= 0
                        && yy >= 0
                        && (xx as usize) < image.width
                        && (yy as usize) < image.height
                    {
                        image.set(yy as usize, xx as usize, colour);
                    }
                }
            }
        }
    }
    fn escape(value: &str) -> String {
        value
            .chars()
            .map(|c| {
                let up: Vec<char> = c.to_uppercase().collect();
                if up.len() == 1 && upper_glyph(up[0]).is_some() {
                    c.to_string()
                } else {
                    format!("U+{:04X}", u32::from(c))
                }
            })
            .collect()
    }
    let axis = match ann.plane {
        "x" => 0usize,
        "y" => 1,
        "z" => 2,
        _ => return Err(MeshError::invalid("invalid section plane")),
    };
    if ann.bbox_mm.iter().flatten().any(|v| !v.is_finite()) || !ann.at_mm.is_finite() {
        return Err(MeshError::invalid("annotation coordinates must be finite"));
    }
    if ann.time_s.is_some_and(|t| !t.is_finite()) {
        return Err(MeshError::invalid("annotation time must be finite"));
    }
    let label = match ann.label {
        Some(l) if !l.is_empty() => l.to_string(),
        _ => ann.field_id.to_string(),
    };
    let units = match ann.units {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => "unspecified".to_string(),
    };
    if label.chars().count() > 1024 || units.chars().count() > 80 {
        return Err(MeshError::invalid("annotation text exceeds bounded layout"));
    }
    let panel_opts = ScalarSectionOptions { draw_legend: false, ..opts.clone() };
    let panel = scalar_section_rgb(values, width, height, &panel_opts)?;
    let (h, w) = (panel.height as i64, panel.width as i64);
    let (left, right, bottom) = (64i64, 16i64, 84i64);
    let title = text_wrap(&escape(&format!("{label} / {units}")), usize::try_from(8.max(w / 6)).unwrap_or(8));
    let top = 12 * title.len() as i64 + 28;
    let (full_w, full_h) = (left + w + right, top + h + bottom);
    if full_w.max(full_h) > 1024 {
        return Err(MeshError::invalid("annotated PNG exceeds 1024 pixel dimension bound"));
    }
    let white = opts.background == Background::White;
    let fill = u8c(if white { WHITE_BACKGROUND } else { OUTSIDE });
    let mut image = RgbImage::filled(full_w as usize, full_h as usize, fill);
    let ink = if white { [18, 28, 40] } else { [226, 233, 240] };
    for r in 0..panel.height {
        for c in 0..panel.width {
            image.set(r + top as usize, c + left as usize, panel.get(r, c));
        }
    }
    for (i, line) in title.iter().enumerate() {
        put(&mut image, left, 6 + 12 * i as i64, line, ink);
    }
    let mut subtitle = format!("{} {} mm", ann.plane.to_uppercase(), fmt_upper_g(ann.at_mm, 5));
    if let Some(t) = ann.time_s {
        let _ = write!(subtitle, " / t {} s", fmt_upper_g(t, 5));
    }
    put(&mut image, left, top - 13, &escape(&subtitle), ink);
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let requested = ann.bbox_mm;
    let mut bx = ann.bbox_mm;
    if let Some(s) = ann.sampled_bounds_mm {
        if s.iter().flatten().any(|x| !x.is_finite()) || s[1][0] <= s[0][0] || s[1][1] <= s[0][1] {
            return Err(MeshError::invalid("sampled annotation bounds must be finite increasing u/v edges"));
        }
        bx[0][u] = s[0][0];
        bx[1][u] = s[1][0];
        bx[0][v] = s[0][1];
        bx[1][v] = s[1][1];
    }
    let names = ['x', 'y', 'z'];
    put(&mut image, 4, top, &escape(&format!("+{} mm", names[v])), ink);
    put(&mut image, 4, top + 13, &fmt_upper_g(bx[1][v], 4), ink);
    put(&mut image, 4, top + h - 8, &fmt_upper_g(bx[0][v], 4), ink);
    put(&mut image, left, top + h + 6, &fmt_upper_g(bx[0][u], 4), ink);
    let high_u = fmt_upper_g(bx[1][u], 4);
    put(&mut image, left + w - 6 * high_u.chars().count() as i64, top + h + 6, &high_u, ink);
    put(&mut image, left + w / 2 - 15, top + h + 18, &escape(&format!("+{} mm", names[u])), ink);
    let (lo, hi) = scalar_section_value_range(values, opts.value_range)?;
    if !(lo + hi).is_finite() {
        return Err(MeshError::invalid("nonfinite color range"));
    }
    let ramp_t = linspace(0.0, 1.0, panel.width);
    let y = top + h + 37;
    for r in py_range(y, y + 8, image.height) {
        for (k, c) in py_range(left, left + w, image.width).enumerate() {
            image.set(r, c, u8c(opts.palette.bar_colour(ramp_t[k], lo, hi)));
        }
    }
    let (mut low, mut high) = (fmt_upper_g(lo, 4), fmt_upper_g(hi, 4));
    #[allow(clippy::float_cmp)]
    if low == high && lo != hi {
        low = fmt_upper_g(lo, 9);
        high = fmt_upper_g(hi, 9);
    }
    put(&mut image, left, y + 12, &low, ink);
    put(&mut image, left + w - 6 * high.chars().count() as i64, y + 12, &high, ink);
    let unit_lines = text_wrap(&escape(&units), usize::try_from(8.max(w / 6)).unwrap_or(8));
    if unit_lines.len() > 2 {
        return Err(MeshError::invalid("unit label exceeds bounded annotation footer"));
    }
    for (i, line) in unit_lines.iter().enumerate() {
        put(&mut image, left, y + 24 + i as i64 * 10, line, ink);
    }
    let record = json!({
        "enabled": true, "full_field_id": ann.field_id, "full_label": label,
        "units": units, "font_encoding": "case_preserving_bitmap_with_U+_escapes",
        "rendered_title_lines": title, "data_panel_rect_px": [left, top, w, h],
        "total_width_px": full_w, "total_height_px": full_h,
        "requested_bbox_mm": requested,
        "effective_color_range": [lo, hi],
        "color_range_tick_labels": [low, high],
        "horizontal_axis": {"name": names[u].to_string(), "direction": "right", "units": "mm",
                            "range": [bx[0][u], bx[1][u]]},
        "vertical_axis": {"name": names[v].to_string(), "direction": "up", "units": "mm",
                          "range": [bx[0][v], bx[1][v]]},
        "normal_axis": {"name": ann.plane, "at_mm": ann.at_mm}, "time_s": ann.time_s,
        "data_panel_unchanged": true,
    });
    Ok((image, record))
}
