// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use rayon::prelude::*;

use super::mesh::SceneMesh;
use super::region::Region;
use super::{SceneError, cross, dot, length, scale, sub};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrthoCamera {
    pub target: [f64; 3],
    pub forward: [f64; 3],
    pub right: [f64; 3],
    pub up: [f64; 3],
    pub px_per_mm: f64,
    pub width: usize,
    pub height: usize,
}

impl OrthoCamera {


    pub fn fit(
        view_direction: [f64; 3],
        up_hint: [f64; 3],
        frame_box: [[f64; 3]; 2],
        width: usize,
        margin: f64,
    ) -> Result<Self, SceneError> {
        let (forward, right, up) = basis(view_direction, up_hint)?;
        if !(0.0..0.45).contains(&margin) || width < 16 {
            return Err(SceneError::Invalid("camera margin must be in [0, 0.45) and width >= 16".into()));
        }
        let centre: [f64; 3] = std::array::from_fn(|a| 0.5 * (frame_box[0][a] + frame_box[1][a]));
        let (mut r0, mut r1, mut u0, mut u1) =
            (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for k in 0..8 {
            let p: [f64; 3] = std::array::from_fn(|a| frame_box[(k >> a) & 1][a]);
            let q = sub(p, centre);
            r0 = r0.min(dot(q, right));
            r1 = r1.max(dot(q, right));
            u0 = u0.min(dot(q, up));
            u1 = u1.max(dot(q, up));
        }
        let (span_r, span_u) = (r1 - r0, u1 - u0);
        if span_r <= 1e-9 || span_u <= 1e-9 {
            return Err(SceneError::Invalid("the frame box has no projected extent".into()));
        }
        let px_per_mm = width as f64 * (1.0 - 2.0 * margin) / span_r;
        let height = ((span_u * px_per_mm) / (1.0 - 2.0 * margin)).ceil().max(16.0) as usize;
        let target: [f64; 3] =
            std::array::from_fn(|a| centre[a] + 0.5 * (r0 + r1) * right[a] + 0.5 * (u0 + u1) * up[a]);
        Ok(Self { target, forward, right, up, px_per_mm, width, height })
    }



    pub fn explicit(
        target: [f64; 3],
        view_direction: [f64; 3],
        up_hint: [f64; 3],
        px_per_mm: f64,
        width: usize,
        height: usize,
    ) -> Result<Self, SceneError> {
        let (forward, right, up) = basis(view_direction, up_hint)?;
        if !(px_per_mm.is_finite() && px_per_mm > 0.0) || width < 16 || height < 16 {
            return Err(SceneError::Invalid(
                "an explicit camera needs px_per_mm > 0 and a raster >= 16 px".into(),
            ));
        }
        Ok(Self { target, forward, right, up, px_per_mm, width, height })
    }

    #[must_use]
    pub fn supersampled(&self, factor: usize) -> Self {
        Self {
            px_per_mm: self.px_per_mm * factor as f64,
            width: self.width * factor,
            height: self.height * factor,
            ..*self
        }
    }

    #[must_use]
    pub fn screen(&self, p: [f64; 3]) -> [f64; 3] {
        let q = sub(p, self.target);
        [
            0.5 * self.width as f64 + dot(q, self.right) * self.px_per_mm,
            0.5 * self.height as f64 - dot(q, self.up) * self.px_per_mm,
            dot(q, self.forward),
        ]
    }

    #[must_use]
    pub fn world(&self, x: f64, y: f64, depth: f64) -> [f64; 3] {
        let r = (x - 0.5 * self.width as f64) / self.px_per_mm;
        let u = (0.5 * self.height as f64 - y) / self.px_per_mm;
        std::array::from_fn(|a| self.target[a] + r * self.right[a] + u * self.up[a] + depth * self.forward[a])
    }
}

type Basis = ([f64; 3], [f64; 3], [f64; 3]);

fn basis(view_direction: [f64; 3], up_hint: [f64; 3]) -> Result<Basis, SceneError> {
    let l = length(view_direction);
    if !(l.is_finite() && l > 1e-12) {
        return Err(SceneError::Invalid("camera view_direction must be a nonzero vector".into()));
    }
    let forward = scale(view_direction, 1.0 / l);
    let r = cross(forward, up_hint);
    let lr = length(r);
    if lr <= 1e-9 * length(up_hint).max(1e-300) {
        return Err(SceneError::Invalid("camera up must not be parallel to the view direction".into()));
    }
    let right = scale(r, 1.0 / lr);
    let up = cross(right, forward);
    Ok((forward, right, up))
}

pub const EMPTY: u8 = u8::MAX;
pub const FLAG_CAP: u8 = 1;
pub const FLAG_BACK: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fragment {
    pub depth: f32,
    pub normal: [i16; 2],
    pub layer: u8,
    pub flags: u8,
}

impl Fragment {
    const NONE: Self = Self { depth: f32::INFINITY, normal: [0, 0], layer: EMPTY, flags: 0 };

    #[must_use]
    pub fn normal(&self) -> [f64; 3] {
        decode_normal(self.normal)
    }
}

#[must_use]
pub fn encode_normal(n: [f64; 3]) -> [i16; 2] {
    let s = n[0].abs() + n[1].abs() + n[2].abs();
    if s <= 0.0 {
        return [0, 0];
    }
    let (mut x, mut y) = (n[0] / s, n[1] / s);
    if n[2] < 0.0 {
        let (ox, oy) = (x, y);
        x = (1.0 - oy.abs()) * ox.signum();
        y = (1.0 - ox.abs()) * oy.signum();
    }
    [(x.clamp(-1.0, 1.0) * 32767.0).round() as i16, (y.clamp(-1.0, 1.0) * 32767.0).round() as i16]
}

#[must_use]
pub fn decode_normal(e: [i16; 2]) -> [f64; 3] {
    let (x, y) = (f64::from(e[0]) / 32767.0, f64::from(e[1]) / 32767.0);
    let z = 1.0 - x.abs() - y.abs();
    let (x, y) = if z < 0.0 { ((1.0 - y.abs()) * x.signum(), (1.0 - x.abs()) * y.signum()) } else { (x, y) };
    let v = [x, y, z];
    let l = length(v);
    if l > 0.0 { scale(v, 1.0 / l) } else { [0.0, 0.0, 1.0] }
}

#[derive(Clone, Debug)]
pub struct GBuffer {
    pub width: usize,
    pub height: usize,
    pub px: Vec<Fragment>,
}

impl GBuffer {
    #[must_use]
    pub fn at(&self, x: usize, y: usize) -> &Fragment {
        &self.px[y * self.width + x]
    }
}

const TIE_MM: f64 = 1e-4;
const EDGE_EPS: f64 = 1e-9;
const BAND_ROWS: usize = 64;

#[must_use]
pub fn rasterize(camera: &OrthoCamera, layers: &[&SceneMesh], region: &Region) -> GBuffer {
    let (w, h) = (camera.width, camera.height);

    let screens: Vec<Vec<[f64; 3]>> = layers
        .iter()
        .map(|m| m.positions.par_iter().map(|p| camera.screen(p.map(f64::from))).collect())
        .collect();
    let mut px = vec![Fragment::NONE; w * h];
    let unbounded = region.is_unbounded();
    px.par_chunks_mut(w * BAND_ROWS).enumerate().for_each(|(band, rows)| {
        let y_lo = band * BAND_ROWS;
        let y_hi = y_lo + rows.len() / w;
        for (li, mesh) in layers.iter().enumerate() {
            let scr = &screens[li];
            for (t, tri) in mesh.triangles.iter().enumerate() {
                let s = tri.map(|i| scr[i as usize]);
                let ymin = s[0][1].min(s[1][1]).min(s[2][1]);
                let ymax = s[0][1].max(s[1][1]).max(s[2][1]);
                if ymax < y_lo as f64 || ymin >= y_hi as f64 {
                    continue;
                }
                let xmin = s[0][0].min(s[1][0]).min(s[2][0]);
                let xmax = s[0][0].max(s[1][0]).max(s[2][0]);
                if xmax < 0.0 || xmin >= w as f64 {
                    continue;
                }
                let den =
                    (s[1][0] - s[0][0]) * (s[2][1] - s[0][1]) - (s[2][0] - s[0][0]) * (s[1][1] - s[0][1]);
                if den.abs() < 1e-12 {
                    continue;
                }
                let c = mesh.corners(t);
                let back = dot(cross(sub(c[1], c[0]), sub(c[2], c[0])), camera.forward) > 0.0;
                let cn = mesh.corner_normals[t];
                let x0 = (xmin - 0.5).ceil().max(0.0) as usize;
                let x1 = ((xmax - 0.5).floor() as i64).min(w as i64 - 1);
                let ya = ((ymin - 0.5).ceil().max(y_lo as f64)) as usize;
                let yb = ((ymax - 0.5).floor() as i64).min(y_hi as i64 - 1);
                if x1 < x0 as i64 || yb < ya as i64 {
                    continue;
                }
                for y in ya..=yb as usize {
                    let py = y as f64 + 0.5;
                    for x in x0..=x1 as usize {
                        let pxc = x as f64 + 0.5;
                        let l1 = ((s[2][0] - pxc) * (s[0][1] - py) - (s[0][0] - pxc) * (s[2][1] - py)) / den;
                        let l2 = ((s[0][0] - pxc) * (s[1][1] - py) - (s[1][0] - pxc) * (s[0][1] - py)) / den;
                        let l0 = 1.0 - l1 - l2;

                        if l0 < -EDGE_EPS || l1 < -EDGE_EPS || l2 < -EDGE_EPS {
                            continue;
                        }
                        let z = l0 * s[0][2] + l1 * s[1][2] + l2 * s[2][2];
                        let cur = &mut rows[(y - y_lo) * w + x];
                        let cz = f64::from(cur.depth);
                        let wins = z < cz - TIE_MM
                            || ((z - cz).abs() <= TIE_MM && back && cur.flags & FLAG_BACK == 0);
                        if !wins {
                            continue;
                        }
                        if !unbounded && !region.contains(camera.world(pxc, py, z)) {
                            continue;
                        }
                        let n: [f64; 3] = std::array::from_fn(|a| {
                            l0 * f64::from(cn[0][a]) + l1 * f64::from(cn[1][a]) + l2 * f64::from(cn[2][a])
                        });
                        let ln = length(n);
                        let n = if ln > 0.0 { scale(n, 1.0 / ln) } else { n };
                        *cur = Fragment {
                            depth: z as f32,
                            normal: encode_normal(n),
                            layer: u8::try_from(li).unwrap_or(EMPTY - 1),
                            flags: if back { FLAG_BACK } else { 0 },
                        };
                    }
                }
            }
        }
    });
    resolve_caps(camera, region, &mut px, w);
    GBuffer { width: w, height: h, px }
}

fn resolve_caps(camera: &OrthoCamera, region: &Region, px: &mut [Fragment], w: usize) {
    let reach = 1e3 + length(camera.target) + (camera.width.max(camera.height) as f64) / camera.px_per_mm;
    px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, f) in row.iter_mut().enumerate() {
            if f.layer == EMPTY || f.flags & FLAG_BACK == 0 {
                continue;
            }
            let (sx, sy) = (x as f64 + 0.5, y as f64 + 0.5);
            let origin = camera.world(sx, sy, -reach);
            let entry = region.cap_entry(origin, camera.forward, f64::from(f.depth) + reach);
            match entry {
                Some(e) => {
                    f.depth = (e.t - reach) as f32;
                    f.normal = encode_normal(e.normal);
                    f.flags = FLAG_CAP | FLAG_BACK;
                }
                None => {
                    f.normal = encode_normal(f.normal().map(|c| -c));
                }
            }
        }
    });
}
