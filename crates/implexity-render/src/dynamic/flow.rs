// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#![allow(clippy::cast_possible_wrap)]

use rayon::prelude::*;

use super::draw::Canvas;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamStyle {
    pub separation: f64,
    pub colour: [u8; 3],
    pub alpha: f64,
    pub arrows: bool,
}

impl Default for StreamStyle {
    fn default() -> Self {
        Self { separation: 14.0, colour: [255, 255, 255], alpha: 0.75, arrows: true }
    }
}

type Sampler<'a> = &'a (dyn Fn(f64, f64) -> Option<(f64, f64)> + Sync);

fn unit(s: Sampler<'_>, x: f64, y: f64) -> Option<(f64, f64)> {
    let (u, v) = s(x, y)?;
    let n = u.hypot(v);
    (n > 1e-300 && n.is_finite()).then(|| (u / n, v / n))
}

struct SeparationGrid {
    cell: f64,
    nx: usize,
    ny: usize,
    points: Vec<Vec<(f64, f64)>>,
}

impl SeparationGrid {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn new(w: usize, h: usize, cell: f64) -> Self {
        let nx = (w as f64 / cell).ceil() as usize + 1;
        let ny = (h as f64 / cell).ceil() as usize + 1;
        Self { cell, nx, ny, points: vec![Vec::new(); nx * ny] }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn key(&self, x: f64, y: f64) -> (usize, usize) {
        (
            ((x / self.cell).max(0.0) as usize).min(self.nx - 1),
            ((y / self.cell).max(0.0) as usize).min(self.ny - 1),
        )
    }

    fn near(&self, x: f64, y: f64, d: f64) -> bool {
        let (i, j) = self.key(x, y);
        for jj in j.saturating_sub(1)..=(j + 1).min(self.ny - 1) {
            for ii in i.saturating_sub(1)..=(i + 1).min(self.nx - 1) {
                if self.points[jj * self.nx + ii].iter().any(|(px, py)| (px - x).hypot(py - y) < d) {
                    return true;
                }
            }
        }
        false
    }

    fn add(&mut self, x: f64, y: f64) {
        let (i, j) = self.key(x, y);
        self.points[j * self.nx + i].push((x, y));
    }
}

#[allow(clippy::too_many_arguments)]
fn trace(
    s: Sampler<'_>,
    grid: &SeparationGrid,
    x0: f64,
    y0: f64,
    dir: f64,
    w: f64,
    h: f64,
    dtest: f64,
) -> Vec<(f64, f64)> {
    let step = 0.5;
    let mut pts = Vec::new();
    let (mut x, mut y) = (x0, y0);
    for _ in 0..4000 {
        let Some((u1, v1)) = unit(s, x, y) else { break };
        let (xm, ym) = (x + 0.5 * step * dir * u1, y + 0.5 * step * dir * v1);
        let Some((u2, v2)) = unit(s, xm, ym) else { break };
        let (xn, yn) = (x + step * dir * u2, y + step * dir * v2);
        if !(0.0..w).contains(&xn) || !(0.0..h).contains(&yn) || grid.near(xn, yn, dtest) {
            break;
        }

        if pts.len() > 20 && (xn - x0).hypot(yn - y0) < 0.5 * dtest {
            break;
        }
        pts.push((xn, yn));
        x = xn;
        y = yn;
    }
    pts
}

#[allow(clippy::cast_possible_truncation)]
pub fn streamlines(
    canvas: &mut Canvas,
    x0: i64,
    y0: i64,
    w: usize,
    h: usize,
    s: Sampler<'_>,
    style: &StreamStyle,
) {
    let dsep = style.separation.max(4.0);
    let dtest = 0.5 * dsep;
    let mut grid = SeparationGrid::new(w, h, dsep);
    let (wf, hf) = (w as f64, h as f64);
    let mut seeds = Vec::new();
    let mut y = 0.5 * dsep;
    while y < hf {
        let mut x = 0.5 * dsep;
        while x < wf {
            seeds.push((x, y));
            x += dsep;
        }
        y += dsep;
    }
    let mut lines: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut k = 0;
    while k < seeds.len() {
        let (sx, sy) = seeds[k];
        k += 1;
        if grid.near(sx, sy, dsep) || unit(s, sx, sy).is_none() {
            continue;
        }
        let mut back = trace(s, &grid, sx, sy, -1.0, wf, hf, dtest);
        let fwd = trace(s, &grid, sx, sy, 1.0, wf, hf, dtest);
        back.reverse();
        back.push((sx, sy));
        back.extend(fwd);
        if back.len() < 12 {
            continue;
        }
        for &(px, py) in &back {
            grid.add(px, py);
        }

        for q in back.iter().step_by(8) {
            if let Some((u, v)) = unit(s, q.0, q.1) {
                seeds.push((q.0 - v * dsep, q.1 + u * dsep));
                seeds.push((q.0 + v * dsep, q.1 - u * dsep));
            }
        }
        lines.push(back);
    }
    let (ox, oy) = (x0 as f64, y0 as f64);
    for line in &lines {
        let shifted: Vec<(f64, f64)> = line.iter().map(|(x, y)| (x + ox, y + oy)).collect();
        canvas.polyline(&shifted, style.colour, style.alpha);
        if style.arrows && shifted.len() > 16 {
            let m = shifted.len() / 2;
            let (a, b) = (shifted[m - 2], shifted[m + 2]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let n = dx.hypot(dy);
            if n > 1e-9 {
                let (ux, uy) = (dx / n, dy / n);
                let len = (0.35 * dsep).clamp(3.0, 7.0);
                let tip = shifted[m];
                for side in [-1.0, 1.0] {
                    let (bx, by) =
                        (tip.0 - len * (ux - side * 0.6 * uy), tip.1 - len * (uy + side * 0.6 * ux));
                    canvas.line(tip.0, tip.1, bx, by, style.colour, style.alpha);
                }
            }
        }
    }
}

fn noise(x: i64, y: i64) -> f64 {

    #[allow(clippy::cast_sign_loss)]
    let mut z =
        (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn lic(w: usize, h: usize, s: Sampler<'_>, length: f64) -> Vec<Option<f64>> {
    let steps = (length.max(2.0) / 0.5) as usize;
    let rows: Vec<Vec<Option<f64>>> = (0..h)
        .into_par_iter()
        .map(|py| {
            (0..w)
                .map(|px| {
                    let (x0, y0) = (px as f64 + 0.5, py as f64 + 0.5);
                    unit(s, x0, y0)?;
                    let mut sum = noise(px as i64, py as i64);
                    let mut count = 1.0;
                    for dir in [-1.0, 1.0] {
                        let (mut x, mut y) = (x0, y0);
                        for _ in 0..steps {
                            let Some((u, v)) = unit(s, x, y) else { break };
                            x += 0.5 * dir * u;
                            y += 0.5 * dir * v;
                            if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 {
                                break;
                            }
                            sum += noise(x.floor() as i64, y.floor() as i64);
                            count += 1.0;
                        }
                    }
                    Some(sum / count)
                })
                .collect()
        })
        .collect();
    let mut out: Vec<Option<f64>> = rows.into_iter().flatten().collect();
    let vals: Vec<f64> = out.iter().flatten().copied().collect();
    if vals.is_empty() {
        return out;
    }
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    let sd = (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64).sqrt().max(1e-9);
    for v in out.iter_mut().flatten() {
        *v = (0.5 + (*v - mean) / (4.0 * sd)).clamp(0.0, 1.0);
    }
    out
}

