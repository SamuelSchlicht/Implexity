// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![allow(clippy::cast_possible_wrap)]

use implexity_mesh::raster::RgbImage;

use super::colormap::{Colormap, Scale};
use super::draw::{Canvas, fmt_num, nice_ticks, text_width};
use super::plane::{MAX_SIDE, Theme};
use crate::RenderError;

pub const CATEGORICAL: [[u8; 3]; 7] = [
    [0, 114, 178],
    [230, 159, 0],
    [0, 158, 115],
    [204, 121, 167],
    [86, 180, 233],
    [213, 94, 0],
    [240, 228, 66],
];

#[derive(Clone, Debug)]
pub struct Series<'a> {
    pub label: String,
    pub x: &'a [f64],
    pub y: &'a [f64],
}

#[derive(Clone, Debug)]
pub struct ChartOptions {
    pub width: usize,
    pub height: usize,
    pub theme: Theme,
    pub title: String,
    pub x_label: String,
    pub y_label: String,
    pub log_y: bool,
    pub cursor_x: Option<f64>,
    pub markers: Vec<(f64, f64, String)>,
}

impl ChartOptions {
    #[must_use]
    pub fn new(title: &str, x_label: &str, y_label: &str) -> Self {
        Self {
            width: 640,
            height: 360,
            theme: Theme::Dark,
            title: title.to_owned(),
            x_label: x_label.to_owned(),
            y_label: y_label.to_owned(),
            log_y: false,
            cursor_x: None,
            markers: Vec::new(),
        }
    }
}

struct Axes {
    x0: f64,
    y0: f64,
    w: f64,
    h: f64,
    xr: (f64, f64),
    yr: (f64, f64),
    log_y: bool,
}

impl Axes {
    fn px(&self, x: f64) -> f64 {
        self.x0 + (x - self.xr.0) / (self.xr.1 - self.xr.0) * self.w
    }

    fn py(&self, y: f64) -> f64 {
        let y = if self.log_y { y.log10() } else { y };
        self.y0 + self.h - (y - self.yr.0) / (self.yr.1 - self.yr.0) * self.h
    }
}

fn padded(lo: f64, hi: f64) -> (f64, f64) {
    if !(lo.is_finite() && hi.is_finite()) {
        return (0.0, 1.0);
    }
    if hi - lo <= 1e-300_f64.max(1e-12 * hi.abs().max(lo.abs())) {
        let w = if lo == 0.0 { 1.0 } else { 0.5 * lo.abs() };
        return (lo - w, hi + w);
    }
    let m = 0.04 * (hi - lo);
    (lo - m, hi + m)
}

fn size_check(width: usize, height: usize) -> Result<(), RenderError> {
    if !(160..=MAX_SIDE).contains(&width) || !(120..=MAX_SIDE).contains(&height) {
        return Err(RenderError::Invalid(format!(
            "chart size must be within 160x120 and {MAX_SIDE}x{MAX_SIDE}"
        )));
    }
    Ok(())
}

#[allow(clippy::cast_possible_truncation)]
fn frame_axes(canvas: &mut Canvas, a: &Axes, o: &ChartOptions) {
    let fg = o.theme.text();
    let grid = match o.theme {
        Theme::Dark => [44, 48, 58],
        Theme::White => [222, 225, 230],
    };
    for t in nice_ticks(a.xr.0, a.xr.1, 6) {
        let x = a.px(t);
        canvas.line(x, a.y0, x, a.y0 + a.h, grid, 1.0);
        let label = fmt_num(t);
        let lw = text_width(&label, 1) as f64;
        canvas.text((x - lw / 2.0) as i64, (a.y0 + a.h + 5.0) as i64, &label, fg, 1);
    }
    for t in nice_ticks(a.yr.0, a.yr.1, 5) {
        let y = a.y0 + a.h - (t - a.yr.0) / (a.yr.1 - a.yr.0) * a.h;
        canvas.line(a.x0, y, a.x0 + a.w, y, grid, 1.0);
        let label = if a.log_y { format!("1E{}", fmt_num(t)) } else { fmt_num(t) };
        let lw = text_width(&label, 1) as f64;
        canvas.text((a.x0 - lw - 5.0) as i64, (y - 3.0) as i64, &label, fg, 1);
    }
    canvas.line(a.x0, a.y0 + a.h, a.x0 + a.w, a.y0 + a.h, fg, 1.0);
    canvas.line(a.x0, a.y0, a.x0, a.y0 + a.h, fg, 1.0);
    canvas.text(8, 6, &o.title, fg, 1);
    let xl = text_width(&o.x_label, 1) as f64;
    canvas.text((a.x0 + a.w / 2.0 - xl / 2.0) as i64, (a.y0 + a.h + 18.0) as i64, &o.x_label, fg, 1);
    canvas.text(8, 18, &o.y_label, fg, 1);
}



#[allow(clippy::cast_possible_truncation)]
pub fn line_chart(series: &[Series<'_>], o: &ChartOptions) -> Result<RgbImage, RenderError> {
    size_check(o.width, o.height)?;
    let mut xs = (f64::INFINITY, f64::NEG_INFINITY);
    let mut ys = (f64::INFINITY, f64::NEG_INFINITY);
    for s in series {
        for (x, y) in s.x.iter().zip(s.y) {
            let y = if o.log_y { if *y > 0.0 { y.log10() } else { f64::NAN } } else { *y };
            if x.is_finite() && y.is_finite() {
                xs = (xs.0.min(*x), xs.1.max(*x));
                ys = (ys.0.min(y), ys.1.max(y));
            }
        }
    }
    if xs.0 > xs.1 {
        return Err(RenderError::Invalid("the chart has no finite points".into()));
    }
    let mut canvas = Canvas::new(o.width, o.height, o.theme.background());
    let a = Axes {
        x0: 64.0,
        y0: 32.0,
        w: o.width as f64 - 64.0 - 16.0,
        h: o.height as f64 - 32.0 - 34.0,
        xr: if xs.1 > xs.0 { xs } else { padded(xs.0, xs.1) },
        yr: padded(ys.0, ys.1),
        log_y: o.log_y,
    };
    frame_axes(&mut canvas, &a, o);
    for (k, s) in series.iter().enumerate() {
        let colour = CATEGORICAL[k % CATEGORICAL.len()];
        let mut run: Vec<(f64, f64)> = Vec::new();
        for (x, y) in s.x.iter().zip(s.y) {
            let ok = x.is_finite() && y.is_finite() && (!o.log_y || *y > 0.0);
            if ok {
                run.push((a.px(*x), a.py(*y)));
            } else if !run.is_empty() {
                canvas.polyline(&run, colour, 1.0);
                run.clear();
            }
        }
        canvas.polyline(&run, colour, 1.0);
        if series.len() > 1 || !s.label.is_empty() {
            let lx = (a.x0 + a.w) as i64 - 150;
            let ly = 34 + 11 * k as i64;
            canvas.rect(lx, ly + 3, 10, 2, colour);
            canvas.text(lx + 14, ly, &s.label, o.theme.text(), 1);
        }
    }
    if let Some(cx) = o.cursor_x.filter(|c| c.is_finite() && *c >= a.xr.0 && *c <= a.xr.1) {
        let x = a.px(cx);
        canvas.line(x, a.y0, x, a.y0 + a.h, [255, 64, 64], 0.9);
    }
    for (x, y, label) in &o.markers {
        if x.is_finite() && y.is_finite() && (!o.log_y || *y > 0.0) {
            let (px, py) = (a.px(*x), a.py(*y));
            canvas.rect(px as i64 - 2, py as i64 - 2, 5, 5, o.theme.text());
            canvas.text(px as i64 + 5, py as i64 - 10, label, o.theme.text(), 1);
        }
    }
    Ok(canvas.img)
}



pub fn phase_portrait(
    x: &[f64],
    y: &[f64],
    phase: Option<&[f64]>,
    o: &ChartOptions,
) -> Result<RgbImage, RenderError> {
    size_check(o.width, o.height)?;
    if x.len() != y.len() || phase.is_some_and(|p| p.len() != x.len()) {
        return Err(RenderError::Invalid("phase portrait series differ in length".into()));
    }
    let fin = |v: &[f64]| {
        v.iter()
            .filter(|a| a.is_finite())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), a| (l.min(*a), h.max(*a)))
    };
    let (xr, yr) = (fin(x), fin(y));
    if xr.0 > xr.1 || yr.0 > yr.1 {
        return Err(RenderError::Invalid("the phase portrait has no finite points".into()));
    }
    let mut canvas = Canvas::new(o.width, o.height, o.theme.background());
    let a = Axes {
        x0: 64.0,
        y0: 32.0,
        w: o.width as f64 - 64.0 - 16.0,
        h: o.height as f64 - 32.0 - 34.0,
        xr: padded(xr.0, xr.1),
        yr: padded(yr.0, yr.1),
        log_y: false,
    };
    frame_axes(&mut canvas, &a, o);
    let n = x.len();
    let map = if phase.is_some() { Colormap::Phase } else { Colormap::Viridis };
    for j in 1..n {
        if [x[j - 1], x[j], y[j - 1], y[j]].iter().all(|v| v.is_finite()) {
            let t = phase.map_or(j as f64 / n as f64, |p| p[j]);
            canvas.line(a.px(x[j - 1]), a.py(y[j - 1]), a.px(x[j]), a.py(y[j]), map.map(t), 1.0);
        }
    }
    Ok(canvas.img)
}



#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn kymograph(
    rows: &[Vec<f64>],
    times: &[f64],
    span: (f64, f64),
    scale: &Scale,
    o: &ChartOptions,
) -> Result<RgbImage, RenderError> {
    size_check(o.width, o.height)?;
    let n = rows.first().map_or(0, Vec::len);
    if rows.is_empty() || n == 0 || rows.iter().any(|r| r.len() != n) || times.len() != rows.len() {
        return Err(RenderError::Invalid(
            "a space-time plot needs equal-length rows with one time each".into(),
        ));
    }
    let mut canvas = Canvas::new(o.width, o.height, o.theme.background());
    let (t0, t1) = (times[0], *times.last().unwrap_or(&times[0]));
    let a = Axes {
        x0: 64.0,
        y0: 32.0,
        w: o.width as f64 - 64.0 - 96.0,
        h: o.height as f64 - 32.0 - 34.0,
        xr: padded_exact(span.0, span.1),
        yr: padded_exact(t0, t1),
        log_y: false,
    };
    let (wp, hp) = (a.w as usize, a.h as usize);
    for py in 0..hp {

        let t = t0 + (py as f64 + 0.5) / hp as f64 * (t1 - t0);
        let k = match times.binary_search_by(|x| x.total_cmp(&t)) {
            Ok(k) => k,
            Err(k) => {
                if k == 0 {
                    0
                } else if k >= times.len() {
                    times.len() - 1
                } else if (t - times[k - 1]) <= (times[k] - t) {
                    k - 1
                } else {
                    k
                }
            }
        };
        for px in 0..wp {
            let j = ((px as f64 + 0.5) / wp as f64 * n as f64) as usize;
            canvas.put(a.x0 as i64 + px as i64, a.y0 as i64 + py as i64, scale.colour(rows[k][j.min(n - 1)]));
        }
    }

    let flipped = Axes { yr: (a.yr.1, a.yr.0), ..a };
    let mut o2 = o.clone();
    o2.log_y = false;
    frame_axes_time_down(&mut canvas, &flipped, &o2);

    let x = (a.x0 + a.w) as i64 + 18;
    let (top, h) = (a.y0 as i64 + 10, a.h as i64 - 10);
    for r in 0..h {
        canvas.rect(x, top + r, 14, 1, scale.map.map(1.0 - r as f64 / (h - 1).max(1) as f64));
    }
    for tick in nice_ticks(scale.lo, scale.hi, 5) {
        let t = (tick - scale.lo) / (scale.hi - scale.lo);
        let y = top + ((1.0 - t) * (h - 1) as f64).round() as i64;
        canvas.rect(x + 14, y, 4, 1, o.theme.text());
        canvas.text(x + 21, y - 3, &fmt_num(tick), o.theme.text(), 1);
    }
    Ok(canvas.img)
}

fn padded_exact(lo: f64, hi: f64) -> (f64, f64) {
    if hi > lo { (lo, hi) } else { padded(lo, hi) }
}

#[allow(clippy::cast_possible_truncation)]
fn frame_axes_time_down(canvas: &mut Canvas, a: &Axes, o: &ChartOptions) {
    let fg = o.theme.text();
    for t in nice_ticks(a.xr.0, a.xr.1, 6) {
        let x = a.px(t);
        let label = fmt_num(t);
        let lw = text_width(&label, 1) as f64;
        canvas.line(x, a.y0 + a.h, x, a.y0 + a.h + 3.0, fg, 1.0);
        canvas.text((x - lw / 2.0) as i64, (a.y0 + a.h + 6.0) as i64, &label, fg, 1);
    }
    let (lo, hi) = (a.yr.1.min(a.yr.0), a.yr.1.max(a.yr.0));
    for t in nice_ticks(lo, hi, 6) {

        let y = a.y0 + (t - lo) / (hi - lo) * a.h;
        let label = fmt_num(t);
        let lw = text_width(&label, 1) as f64;
        canvas.line(a.x0 - 3.0, y, a.x0, y, fg, 1.0);
        canvas.text((a.x0 - lw - 5.0) as i64, (y - 3.0) as i64, &label, fg, 1);
    }
    canvas.text(8, 6, &o.title, fg, 1);
    let xl = text_width(&o.x_label, 1) as f64;
    canvas.text((a.x0 + a.w / 2.0 - xl / 2.0) as i64, (a.y0 + a.h + 18.0) as i64, &o.x_label, fg, 1);
    canvas.text(8, 18, &o.y_label, fg, 1);
}

