// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![allow(clippy::cast_possible_wrap)]

use implexity_mesh::raster::{RgbImage, upper_glyph};

fn extra_glyph(c: char) -> Option<[&'static str; 7]> {
    Some(match c {
        '(' => ["00010", "00100", "01000", "01000", "01000", "00100", "00010"],
        ')' => ["01000", "00100", "00010", "00010", "00010", "00100", "01000"],
        '[' => ["01110", "01000", "01000", "01000", "01000", "01000", "01110"],
        ']' => ["01110", "00010", "00010", "00010", "00010", "00010", "01110"],
        '=' => ["00000", "00000", "11111", "00000", "11111", "00000", "00000"],
        '%' => ["11001", "11010", "00010", "00100", "01000", "01011", "10011"],
        ',' => ["00000", "00000", "00000", "00000", "00110", "00100", "01000"],
        '*' => ["00000", "10101", "01110", "11111", "01110", "10101", "00000"],
        '^' => ["00100", "01010", "10001", "00000", "00000", "00000", "00000"],
        '<' => ["00010", "00100", "01000", "10000", "01000", "00100", "00010"],
        '>' => ["01000", "00100", "00010", "00001", "00010", "00100", "01000"],
        '|' => ["00100", "00100", "00100", "00100", "00100", "00100", "00100"],
        '#' => ["01010", "11111", "01010", "01010", "11111", "01010", "00000"],
        '\'' => ["00100", "00100", "00000", "00000", "00000", "00000", "00000"],
        _ => return None,
    })
}

#[must_use]
pub fn text_width(text: &str, scale: usize) -> usize {
    let n = text.chars().count();
    if n == 0 { 0 } else { (6 * n - 1) * scale }
}

#[derive(Clone, Debug)]
pub struct Canvas {
    pub img: RgbImage,
}

impl Canvas {
    #[must_use]
    pub fn new(width: usize, height: usize, bg: [u8; 3]) -> Self {
        Self { img: RgbImage::filled(width.max(1), height.max(1), bg) }
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.img.width
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.img.height
    }

    pub fn put(&mut self, x: i64, y: i64, c: [u8; 3]) {
        if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y))
            && x < self.img.width
            && y < self.img.height
        {
            self.img.set(y, x, c);
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn blend(&mut self, x: i64, y: i64, c: [u8; 3], a: f64) {
        if a <= 0.0 {
            return;
        }
        if let (Ok(xu), Ok(yu)) = (usize::try_from(x), usize::try_from(y))
            && xu < self.img.width
            && yu < self.img.height
        {
            let a = a.min(1.0);
            let old = self.img.get(yu, xu);
            let mix = [0, 1, 2].map(|i| (f64::from(old[i]) * (1.0 - a) + f64::from(c[i]) * a).round() as u8);
            self.img.set(yu, xu, mix);
        }
    }

    pub fn rect(&mut self, x0: i64, y0: i64, w: i64, h: i64, c: [u8; 3]) {
        for y in y0.max(0)..(y0 + h).min(self.img.height as i64) {
            for x in x0.max(0)..(x0 + w).min(self.img.width as i64) {
                self.put(x, y, c);
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn line(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, c: [u8; 3], alpha: f64) {
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            return;
        }
        let steep = (y1 - y0).abs() > (x1 - x0).abs();
        let (mut ax, mut ay, mut bx, mut by) = if steep { (y0, x0, y1, x1) } else { (x0, y0, x1, y1) };
        if ax > bx {
            std::mem::swap(&mut ax, &mut bx);
            std::mem::swap(&mut ay, &mut by);
        }
        let dx = bx - ax;
        let gradient = if dx.abs() < 1e-12 { 1.0 } else { (by - ay) / dx };
        let mut plot = |x: i64, y: i64, a: f64| {
            if steep { self.blend(y, x, c, a * alpha) } else { self.blend(x, y, c, a * alpha) }
        };
        let (xs, xe) = (ax.round() as i64, bx.round() as i64);
        if xe - xs > 100_000 {
            return;
        }
        let mut y = ay + gradient * (xs as f64 - ax);
        for x in xs..=xe {
            let f = y - y.floor();
            plot(x, y.floor() as i64, 1.0 - f);
            plot(x, y.floor() as i64 + 1, f);
            y += gradient;
        }
    }

    pub fn polyline(&mut self, pts: &[(f64, f64)], c: [u8; 3], alpha: f64) {
        for w in pts.windows(2) {
            self.line(w[0].0, w[0].1, w[1].0, w[1].1, c, alpha);
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn triangle(&mut self, p: [(f64, f64); 3], v: [f64; 3], colour: &dyn Fn(f64) -> [u8; 3]) {
        let area = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[2].0 - p[0].0) * (p[1].1 - p[0].1);
        if area.abs() < 1e-12 || !p.iter().all(|q| q.0.is_finite() && q.1.is_finite()) {
            return;
        }
        let xmin = p.iter().map(|q| q.0).fold(f64::INFINITY, f64::min).floor().max(0.0) as i64;
        let xmax =
            p.iter().map(|q| q.0).fold(f64::NEG_INFINITY, f64::max).ceil().min(self.img.width as f64 - 1.0)
                as i64;
        let ymin = p.iter().map(|q| q.1).fold(f64::INFINITY, f64::min).floor().max(0.0) as i64;
        let ymax =
            p.iter().map(|q| q.1).fold(f64::NEG_INFINITY, f64::max).ceil().min(self.img.height as f64 - 1.0)
                as i64;
        for y in ymin..=ymax {
            for x in xmin..=xmax {
                let (px, py) = (x as f64, y as f64);
                let w0 = ((p[1].0 - px) * (p[2].1 - py) - (p[2].0 - px) * (p[1].1 - py)) / area;
                let w1 = ((p[2].0 - px) * (p[0].1 - py) - (p[0].0 - px) * (p[2].1 - py)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 >= -1e-9 && w1 >= -1e-9 && w2 >= -1e-9 {
                    self.put(x, y, colour(w0 * v[0] + w1 * v[1] + w2 * v[2]));
                }
            }
        }
    }

    pub fn text(&mut self, x: i64, y: i64, text: &str, c: [u8; 3], scale: usize) {
        let s = i64::try_from(scale.max(1)).unwrap_or(1);
        let mut cursor = x;
        for ch in text.to_uppercase().chars() {
            let glyph = upper_glyph(ch).or_else(|| extra_glyph(ch)).unwrap_or(["00000"; 7]);
            for (gy, row) in glyph.iter().enumerate() {
                for (gx, bit) in row.chars().enumerate() {
                    if bit == '1' {
                        self.rect(cursor + gx as i64 * s, y + gy as i64 * s, s, s, c);
                    }
                }
            }
            cursor += 6 * s;
        }
    }

    pub fn text_chip(&mut self, x: i64, y: i64, text: &str, fg: [u8; 3], bg: [u8; 3], scale: usize) {
        let w = i64::try_from(text_width(text, scale)).unwrap_or(0);
        let h = 7 * i64::try_from(scale.max(1)).unwrap_or(1);
        self.rect(x - 2, y - 2, w + 4, h + 4, bg);
        self.text(x, y, text, fg, scale);
    }

    pub fn blit(&mut self, other: &RgbImage, x: i64, y: i64) {
        for r in 0..other.height {
            for c in 0..other.width {
                self.put(x + c as i64, y + r as i64, other.get(r, c));
            }
        }
    }
}

#[must_use]
pub fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return "NAN".into();
    }
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    if (1e-3..1e5).contains(&a) {
        let digits = if a >= 100.0 {
            1
        } else if a >= 1.0 {
            3
        } else {
            4
        };
        let s = format!("{v:.digits$}");
        let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s };
        return s;
    }
    let s = format!("{v:.2e}");

    let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
    let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
    format!("{m}E{e}")
}

#[must_use]
pub fn nice_ticks(lo: f64, hi: f64, target: usize) -> Vec<f64> {
    if !(lo.is_finite() && hi.is_finite()) || hi <= lo {
        return vec![lo];
    }
    let raw = (hi - lo) / target.max(2) as f64;
    let exponent = raw.log10().floor();
    let mag = 10f64.powf(exponent);
    let norm = raw / mag;
    let mantissa = if norm < 1.5 {
        1.0
    } else if norm < 3.0 {
        2.0
    } else if norm < 7.0 {
        5.0
    } else {
        10.0
    };
    let step = mantissa * mag;

    let value =
        |k: f64| if exponent < 0.0 { k * mantissa / 10f64.powf(-exponent) } else { k * mantissa * mag };
    let mut k = (lo / step).ceil();
    let mut out = Vec::new();
    while value(k) <= hi + 1e-9 * step && out.len() < 50 {
        out.push(value(k) + 0.0);
        k += 1.0;
    }
    out
}

