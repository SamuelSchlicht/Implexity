// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![allow(clippy::cast_possible_wrap)]

use super::colormap::Scale;
use super::draw::{Canvas, fmt_num, nice_ticks, text_width};
use super::flow::{StreamStyle, lic, streamlines};
use super::grid::{GridField, Reduce};
use super::solid::Triangles;
use crate::RenderError;
use implexity_mesh::raster::RgbImage;
use rayon::prelude::*;

pub const MAX_SIDE: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    White,
}

impl Theme {


    pub fn parse(s: &str) -> Result<Self, RenderError> {
        match s {
            "dark" => Ok(Self::Dark),
            "white" => Ok(Self::White),
            other => Err(RenderError::Invalid(format!("background must be dark or white, not {other:?}"))),
        }
    }

    #[must_use]
    pub const fn background(self) -> [u8; 3] {
        match self {
            Self::Dark => [12, 14, 18],
            Self::White => [255, 255, 255],
        }
    }

    #[must_use]
    pub const fn text(self) -> [u8; 3] {
        match self {
            Self::Dark => [226, 230, 236],
            Self::White => [24, 28, 34],
        }
    }

    #[must_use]
    pub const fn void(self) -> [u8; 3] {
        match self {
            Self::Dark => [28, 31, 38],
            Self::White => [236, 238, 241],
        }
    }

    #[must_use]
    pub const fn line(self) -> [u8; 3] {
        match self {
            Self::Dark => [255, 255, 255],
            Self::White => [0, 0, 0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub lo: [f64; 2],
    pub hi: [f64; 2],
}

impl Window {
    #[must_use]
    pub fn of(f: &GridField) -> Self {
        let (lo, hi) = f.bounds();
        Self { lo: [lo[0], lo[1]], hi: [hi[0], hi[1]] }
    }

    #[must_use]
    pub fn padded(self, pad: f64) -> Self {
        Self { lo: [self.lo[0] - pad, self.lo[1] - pad], hi: [self.hi[0] + pad, self.hi[1] + pad] }
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            lo: [self.lo[0].min(other.lo[0]), self.lo[1].min(other.lo[1])],
            hi: [self.hi[0].max(other.hi[0]), self.hi[1].max(other.hi[1])],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SolidLayer<'a> {
    pub displacement: &'a GridField,
    pub scale: f64,
    pub mask: Option<&'a GridField>,
    pub colour: Option<(&'a GridField, Scale)>,
    pub grid_every: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct BodyLayer<'a> {
    pub triangles: &'a Triangles,
    pub axes: [usize; 2],
    pub colour: Option<Scale>,
    pub label: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub enum FlowOverlay<'a> {
    Streamlines(&'a GridField, StreamStyle),
    Lic(&'a GridField, f64),
}

#[derive(Clone, Copy, Debug)]
pub struct Layers<'a> {
    pub base: Option<(&'a GridField, Reduce, Scale)>,
    pub occupancy: Option<(&'a GridField, bool)>,
    pub flow: Option<FlowOverlay<'a>>,
    pub solid: Option<SolidLayer<'a>>,
    pub body: Option<BodyLayer<'a>>,
}

#[derive(Clone, Debug)]
pub struct FrameText {
    pub title: String,
    pub subtitle: String,
    pub bar_label: String,
}

#[derive(Clone, Copy, Debug)]
pub struct FrameSize {
    pub width: usize,
    pub height: usize,
    pub theme: Theme,
    pub colorbar: bool,
}

const TOP: usize = 30;
const BAR: usize = 86;
const PAD: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub width: usize,
    pub height: usize,
    pub x0: usize,
    pub y0: usize,
    pub w: usize,
    pub h: usize,
    pub pixel: f64,
    pub window: Window,
}

impl Geometry {


    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn fit(window: Window, size: &FrameSize) -> Result<Self, RenderError> {
        Self::fit_bars(window, size, usize::from(size.colorbar))
    }



    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn fit_bars(window: Window, size: &FrameSize, bars: usize) -> Result<Self, RenderError> {
        let (sx, sy) = (window.hi[0] - window.lo[0], window.hi[1] - window.lo[1]);
        if !(sx > 0.0 && sy > 0.0 && sx.is_finite() && sy.is_finite()) {
            return Err(RenderError::Invalid("the frame window is empty".into()));
        }
        let bar = BAR * bars;
        let aw = size.width.min(MAX_SIDE).saturating_sub(2 * PAD + bar);
        let ah = size.height.min(MAX_SIDE).saturating_sub(TOP + PAD);
        if aw < 16 || ah < 16 {
            return Err(RenderError::Invalid("the frame size leaves no room for the plot".into()));
        }
        let pixel = (sx / aw as f64).max(sy / ah as f64);
        let w = ((sx / pixel).round() as usize).clamp(1, aw);
        let h = ((sy / pixel).round() as usize).clamp(1, ah);
        Ok(Self { width: w + 2 * PAD + bar, height: h + TOP + PAD, x0: PAD, y0: TOP, w, h, pixel, window })
    }

    #[must_use]
    pub fn to_world(&self, px: f64, py: f64) -> [f64; 2] {
        [self.window.lo[0] + px * self.pixel, self.window.hi[1] - py * self.pixel]
    }

    #[must_use]
    pub fn to_pixel(&self, p: [f64; 2]) -> (f64, f64) {
        (
            self.x0 as f64 + (p[0] - self.window.lo[0]) / self.pixel - 0.5,
            self.y0 as f64 + (self.window.hi[1] - p[1]) / self.pixel - 0.5,
        )
    }
}

#[must_use]
pub fn contour_segments(f: &GridField, level: f64) -> Vec<([f64; 2], [f64; 2])> {
    let (nx, ny) = (f.shape[0], f.shape[1]);
    let c = f.components;
    let val = |i: usize, j: usize| f.values[(i * ny + j) * c];
    let pos = |i: f64, j: f64| [f.origin[0] + i * f.spacing[0], f.origin[1] + j * f.spacing[1]];
    let mut out = Vec::new();
    for i in 0..nx.saturating_sub(1) {
        for j in 0..ny.saturating_sub(1) {
            let v = [val(i, j), val(i + 1, j), val(i + 1, j + 1), val(i, j + 1)];
            if v.iter().any(|x| !x.is_finite()) {
                continue;
            }
            let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
            let mut pts = Vec::with_capacity(4);
            for e in 0..4 {
                let (a, b) = (e, (e + 1) % 4);
                if (v[a] >= level) != (v[b] >= level) {
                    let s = (level - v[a]) / (v[b] - v[a]);
                    let (ca, cb) = (corners[a], corners[b]);
                    pts.push(pos(i as f64 + ca.0 + s * (cb.0 - ca.0), j as f64 + ca.1 + s * (cb.1 - ca.1)));
                }
            }
            match pts.len() {
                2 => out.push((pts[0], pts[1])),
                4 => {

                    let centre = 0.25 * v.iter().sum::<f64>();
                    if (centre >= level) == (v[0] >= level) {
                        out.push((pts[0], pts[3]));
                        out.push((pts[1], pts[2]));
                    } else {
                        out.push((pts[0], pts[1]));
                        out.push((pts[2], pts[3]));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn colorbar(canvas: &mut Canvas, g: &Geometry, scale: &Scale, label: &str, theme: Theme, slot: usize) {
    let x = i64::try_from(g.x0 + g.w + 14 + slot * BAR).unwrap_or(0);
    let top = i64::try_from(g.y0 + 12).unwrap_or(0);
    let h = i64::try_from(g.h.saturating_sub(18).max(20)).unwrap_or(20);
    for r in 0..h {
        let t = 1.0 - r as f64 / (h - 1).max(1) as f64;
        let c = scale.map.map(t);
        canvas.rect(x, top + r, 14, 1, c);
    }
    let fg = theme.text();
    for tick in nice_ticks(scale.lo, scale.hi, 5) {
        let t = (tick - scale.lo) / (scale.hi - scale.lo);
        #[allow(clippy::cast_possible_truncation)]
        let y = top + ((1.0 - t) * (h - 1) as f64).round() as i64;
        canvas.rect(x + 14, y, 4, 1, fg);
        canvas.text(x + 21, y - 3, &fmt_num(tick), fg, 1);
    }
    let lw = i64::try_from(text_width(label, 1)).unwrap_or(0);
    canvas.text((x + 7 - lw / 2).max(x - 10), top - 10, label, fg, 1);
}



#[allow(clippy::too_many_lines, clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub fn render_frame(
    layers: &Layers<'_>,
    window: Window,
    size: &FrameSize,
    text: &FrameText,
) -> Result<RgbImage, RenderError> {
    for f in [layers.base.map(|b| b.0), layers.occupancy.map(|o| o.0), layers.solid.map(|s| s.displacement)]
        .into_iter()
        .flatten()
    {
        if f.dims() != 2 {
            return Err(RenderError::Invalid(
                "frame layers must be 2-D fields (slice 3-D fields first)".into(),
            ));
        }
    }
    let body_bar = layers.body.and_then(|b| b.colour.map(|s| (s, b.label)));
    let bars = if size.colorbar {
        usize::from(layers.base.is_some() || layers.solid.is_some_and(|s| s.colour.is_some()))
            + usize::from(body_bar.is_some())
    } else {
        0
    };
    let g = Geometry::fit_bars(window, size, bars.min(2).max(usize::from(size.colorbar)))?;
    let theme = size.theme;
    let mut canvas = Canvas::new(g.width, g.height, theme.background());

    let mut base_rgb: Vec<Option<[u8; 3]>> = vec![None; g.w * g.h];
    if let Some((field, reduce, scale)) = layers.base {
        base_rgb.par_chunks_mut(g.w.max(1)).enumerate().for_each(|(py, row)| {
            for (px, out) in row.iter_mut().enumerate() {
                let p = g.to_world(px as f64 + 0.5, py as f64 + 0.5);
                *out = field.sample_scalar(&p, reduce).map(|v| scale.colour(v));
            }
        });
    }
    if let Some(FlowOverlay::Lic(vf, length)) = layers.flow {
        let s = |x: f64, y: f64| {
            let p = g.to_world(x, y);
            vf.sample2(&p).map(|v| (v[0], -v[1]))
        };
        let tex = lic(g.w, g.h, &s, length);
        for (c, t) in base_rgb.iter_mut().zip(&tex) {
            if let Some(t) = t {
                let rgb = c.unwrap_or([200, 200, 200]);
                let k = 0.25 + 0.95 * t;
                #[allow(clippy::cast_sign_loss)]
                let shade = |v: u8| (f64::from(v) * k).round().clamp(0.0, 255.0) as u8;
                *c = Some(rgb.map(shade));
            }
        }
    }
    let void = theme.void();
    for py in 0..g.h {
        for px in 0..g.w {
            canvas.put((g.x0 + px) as i64, (g.y0 + py) as i64, base_rgb[py * g.w + px].unwrap_or(void));
        }
    }

    if let Some((occ, fill)) = layers.occupancy {
        if fill {
            for py in 0..g.h {
                for px in 0..g.w {
                    let p = g.to_world(px as f64 + 0.5, py as f64 + 0.5);
                    if occ.sample_scalar(&p, Reduce::Component(0)).is_some_and(|o| o >= 0.5) {
                        canvas.blend((g.x0 + px) as i64, (g.y0 + py) as i64, [150, 156, 166], 0.85);
                    }
                }
            }
        }
        for (a, b) in contour_segments(occ, 0.5) {
            let (pa, pb) = (g.to_pixel(a), g.to_pixel(b));
            canvas.line(pa.0, pa.1, pb.0, pb.1, theme.line(), 0.95);
        }
    }

    if let Some(FlowOverlay::Streamlines(vf, style)) = layers.flow {
        let s = |x: f64, y: f64| {
            let p = g.to_world(x, y);
            let blocked = layers
                .occupancy
                .and_then(|(o, _)| o.sample_scalar(&p, Reduce::Component(0)))
                .is_some_and(|o| o >= 0.5);
            if blocked { None } else { vf.sample2(&p).map(|v| (v[0], -v[1])) }
        };
        let style =
            StreamStyle { colour: if theme == Theme::Dark { style.colour } else { [20, 20, 20] }, ..style };
        streamlines(&mut canvas, g.x0 as i64, g.y0 as i64, g.w, g.h, &s, &style);
    }

    if let Some(solid) = layers.solid {
        let d = solid.displacement;
        let (nx, ny) = (d.shape[0], d.shape[1]);
        let node = |i: usize, j: usize| -> Option<[f64; 2]> {
            let k = (i * ny + j) * d.components;
            let (u, v) = (d.values[k], d.values[k + 1]);
            (u.is_finite() && v.is_finite()).then(|| {
                [
                    d.origin[0] + i as f64 * d.spacing[0] + solid.scale * u,
                    d.origin[1] + j as f64 * d.spacing[1] + solid.scale * v,
                ]
            })
        };
        let inside = |i: usize, j: usize| {
            solid.mask.is_none_or(|m| m.values.get((i * ny + j) * m.components).is_some_and(|o| *o >= 0.5))
        };
        let value = |i: usize, j: usize| {
            solid
                .colour
                .map_or(0.0, |(f, _)| f.values.get((i * ny + j) * f.components).copied().unwrap_or(f64::NAN))
        };
        let grey = [170u8, 176, 186];
        let colour = |v: f64| solid.colour.map_or(grey, |(_, s)| s.colour(v));
        for i in 0..nx.saturating_sub(1) {
            for j in 0..ny.saturating_sub(1) {
                let quad = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
                let solid_cells = quad.iter().filter(|(a, b)| inside(*a, *b)).count();
                if solid_cells < 3 {
                    continue;
                }
                let pts: Vec<Option<[f64; 2]>> = quad.iter().map(|(a, b)| node(*a, *b)).collect();
                if pts.iter().any(Option::is_none) {
                    continue;
                }
                let px: Vec<(f64, f64)> = pts.iter().flatten().map(|p| g.to_pixel(*p)).collect();
                let vals: Vec<f64> = quad.iter().map(|(a, b)| value(*a, *b)).collect();
                if solid_cells == 4 {
                    canvas.triangle([px[0], px[1], px[2]], [vals[0], vals[1], vals[2]], &colour);
                    canvas.triangle([px[0], px[2], px[3]], [vals[0], vals[2], vals[3]], &colour);
                } else {
                    let keep: Vec<usize> = (0..4).filter(|&k| inside(quad[k].0, quad[k].1)).collect();
                    canvas.triangle(
                        [px[keep[0]], px[keep[1]], px[keep[2]]],
                        [vals[keep[0]], vals[keep[1]], vals[keep[2]]],
                        &colour,
                    );
                }
            }
        }
        if solid.grid_every > 0 {
            let e = solid.grid_every;
            let fg = theme.line();
            for i in (0..nx).step_by(e) {
                let line: Vec<(f64, f64)> = (0..ny)
                    .filter(|&j| inside(i, j))
                    .filter_map(|j| node(i, j))
                    .map(|p| g.to_pixel(p))
                    .collect();
                canvas.polyline(&line, fg, 0.35);
            }
            for j in (0..ny).step_by(e) {
                let line: Vec<(f64, f64)> = (0..nx)
                    .filter(|&i| inside(i, j))
                    .filter_map(|i| node(i, j))
                    .map(|p| g.to_pixel(p))
                    .collect();
                canvas.polyline(&line, fg, 0.35);
            }
        }

        if let Some(mask) = solid.mask {
            for (a, b) in contour_segments(mask, 0.5) {
                let map = |p: [f64; 2]| -> Option<[f64; 2]> {
                    let u = d.sample2(&p)?;
                    Some([p[0] + solid.scale * u[0], p[1] + solid.scale * u[1]])
                };
                if let (Some(a), Some(b)) = (map(a), map(b)) {
                    let (pa, pb) = (g.to_pixel(a), g.to_pixel(b));
                    canvas.line(pa.0, pa.1, pb.0, pb.1, theme.line(), 0.95);
                }
            }
        }
    }

    if let Some(body) = layers.body {
        let grey = [170u8, 176, 186];
        let colour = |v: f64| match body.colour {
            Some(s) if v.is_finite() => s.colour(v),
            _ => grey,
        };
        let [a, b] = body.axes;
        let to_px = |p: &[f64; 3]| g.to_pixel([p[a], p[b]]);
        for (t, v) in body.triangles.points.iter().zip(&body.triangles.values) {
            canvas.triangle([to_px(&t[0]), to_px(&t[1]), to_px(&t[2])], *v, &colour);
        }
        for [p, q] in &body.triangles.outline {
            let (pa, pb) = (to_px(p), to_px(q));
            canvas.line(pa.0, pa.1, pb.0, pb.1, theme.line(), 0.95);
        }
    }

    let fg = theme.text();
    canvas.text(PAD as i64, 5, &text.title, fg, 1);
    canvas.text(PAD as i64, 16, &text.subtitle, fg, 1);
    if size.colorbar {
        let mut slot = 0;
        if let Some((_, _, scale)) = layers.base {
            colorbar(&mut canvas, &g, &scale, &text.bar_label, theme, 0);
            slot = 1;
        } else if let Some(SolidLayer { colour: Some((_, scale)), .. }) = layers.solid {
            colorbar(&mut canvas, &g, &scale, &text.bar_label, theme, 0);
            slot = 1;
        }
        if let Some((scale, label)) = body_bar {
            colorbar(&mut canvas, &g, &scale, label, theme, slot);
        }
    }
    Ok(canvas.img)
}

