// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use rayon::prelude::*;

use super::field::{Colormap, GridField};
use super::raster::{EMPTY, FLAG_CAP, GBuffer, OrthoCamera, rasterize};
use super::{SceneMesh, dot, length, scale};
use crate::mesh_scene::region::Region;

#[derive(Clone, Debug, PartialEq)]
pub enum Colouring {
    Uniform([f64; 3]),
    Field {
        field: usize,
        map: Colormap,
        range: [f64; 2],
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerStyle {
    pub colouring: Colouring,
    pub cap_rgb: Option<[f64; 3]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hatch {
    pub spacing_px: f64,
    pub width_px: f64,
    pub rgb: [f64; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadeOptions {
    pub shadows: bool,
    pub ambient_occlusion: bool,
    pub ao_radius_mm: f64,
    pub outline: bool,
    pub hatch: Option<Hatch>,
    pub background: [f64; 3],
}

fn to_linear(c: [f64; 3]) -> [f64; 3] {
    c.map(|v| (v.clamp(0.0, 255.0) / 255.0).powf(2.2))
}

fn to_display(c: [f64; 3]) -> [u8; 3] {
    c.map(|v| (255.0 * v.clamp(0.0, 1.0).powf(1.0 / 2.2)).round() as u8)
}

fn normalise(v: [f64; 3]) -> [f64; 3] {
    let l = length(v);
    if l > 0.0 { scale(v, 1.0 / l) } else { v }
}

fn mix(a: [f64; 3], b: [f64; 3], s: f64) -> [f64; 3] {
    std::array::from_fn(|i| a[i] + s * (b[i] - a[i]))
}

fn rig(camera: &OrthoCamera, r: f64, u: f64, f: f64) -> [f64; 3] {
    normalise(std::array::from_fn(|a| r * camera.right[a] + u * camera.up[a] + f * camera.forward[a]))
}

#[must_use]
pub fn key_light(camera: &OrthoCamera) -> [f64; 3] {
    rig(camera, -0.55, 0.9, -0.6)
}

pub struct ShadowMap {
    camera: OrthoCamera,
    buffer: GBuffer,
    bias_mm: f64,
}

impl ShadowMap {


    pub fn build(
        light: [f64; 3],
        frame_box: [[f64; 3]; 2],
        layers: &[&SceneMesh],
        region: &Region,
        resolution: usize,
    ) -> Result<Self, super::SceneError> {
        let up_hint = if light[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [0.0, 1.0, 0.0] };
        let camera = OrthoCamera::fit(light.map(|c| -c), up_hint, frame_box, resolution, 0.02)?;
        let buffer = rasterize(&camera, layers, region);
        Ok(Self { camera, buffer, bias_mm: 2.5 / camera.px_per_mm })
    }

    fn lit(&self, p: [f64; 3], slope: f64) -> f64 {
        let s = self.camera.screen(p);
        let (cx, cy) = (s[0].floor() as i64, s[1].floor() as i64);
        let bias = self.bias_mm * (1.0 + 2.0 * slope);
        let (w, h) = (self.buffer.width as i64, self.buffer.height as i64);
        let mut lit = 0.0;
        for dy in -2..=2 {
            for dx in -2..=2 {
                let (x, y) = (cx + dx, cy + dy);
                if x < 0 || y < 0 || x >= w || y >= h {
                    lit += 1.0;
                    continue;
                }
                let f = self.buffer.at(x as usize, y as usize);
                if f.layer == EMPTY || s[2] <= f64::from(f.depth) + bias {
                    lit += 1.0;
                }
            }
        }
        lit / 25.0
    }
}

pub struct ShadeInput<'a> {
    pub gbuffer: &'a GBuffer,
    pub camera: &'a OrthoCamera,
    pub supersample: usize,
    pub styles: &'a [LayerStyle],
    pub fields: &'a [&'a GridField],
    pub shadow: Option<&'a ShadowMap>,
    pub options: ShadeOptions,
}

const AO_SAMPLES: usize = 20;
const BAYER4: [f64; 16] =
    [0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0];

#[must_use]
pub fn shade(input: &ShadeInput<'_>) -> Vec<u8> {
    let g = input.gbuffer;
    let cam = input.camera;
    let ss = input.supersample.max(1);
    let (fw, fh) = (g.width / ss, g.height / ss);
    let key = key_light(cam);
    let fill = rig(cam, 0.75, 0.15, -0.6);
    let view = cam.forward.map(|c| -c);
    let half = normalise(std::array::from_fn(|a| key[a] + view[a]));
    let background = to_linear(input.options.background);
    let line_rgb = to_linear([40.0, 42.0, 46.0]);
    let mm_per_px = 1.0 / cam.px_per_mm;
    let depth_tol = (6.0 * mm_per_px).max(0.03);
    let ao_r_px = input.options.ao_radius_mm * cam.px_per_mm;
    let world = |x: usize, y: usize, d: f32| cam.world(x as f64 + 0.5, y as f64 + 0.5, f64::from(d));
    let mut out = vec![0_u8; fw * fh * 3];
    out.par_chunks_mut(fw * 3).enumerate().for_each(|(fy, row)| {
        for fx in 0..fw {
            let mut acc = [0.0; 3];
            for sy in 0..ss {
                for sx in 0..ss {
                    let (x, y) = (fx * ss + sx, fy * ss + sy);
                    let f = g.at(x, y);
                    let mut c = if f.layer == EMPTY {
                        background
                    } else {
                        let p = world(x, y, f.depth);
                        let mut n = f.normal();
                        if dot(n, view) < 0.0 {
                            n = n.map(|v| -v);
                        }
                        let cap = f.flags & FLAG_CAP != 0;
                        let style = &input.styles[f.layer as usize];
                        let albedo = match (&style.colouring, style.cap_rgb, cap) {
                            (_, Some(rgb), true) | (&Colouring::Uniform(rgb), _, _) => to_linear(rgb),
                            (Colouring::Field { field, map, range }, _, _) => {
                                let v = input.fields[*field].sample(p);
                                to_linear(map.at((v - range[0]) / (range[1] - range[0])))
                            }
                        };
                        let ao = if input.options.ambient_occlusion && ao_r_px >= 1.0 {
                            ambient_occlusion(g, cam, x, y, p, n, ao_r_px, input.options.ao_radius_mm)
                        } else {
                            1.0
                        };
                        let nl = dot(n, key);
                        let sh = match input.shadow {
                            Some(s) if nl > 0.0 => s.lit(p, (1.0 - nl * nl).max(0.0).sqrt() / nl.max(0.2)),
                            _ => 1.0,
                        };
                        let wrap = |d: f64| ((d + 0.1) / 1.1).max(0.0);
                        let ambient = (0.36 + 0.08 * dot(n, cam.up)) * ao;
                        let diffuse = 0.74 * wrap(nl) * (0.4 + 0.6 * sh) + 0.34 * wrap(dot(n, fill));
                        let spec = if cap { 0.0 } else { 0.10 * dot(n, half).max(0.0).powf(48.0) * sh };
                        let mut c: [f64; 3] = std::array::from_fn(|a| albedo[a] * (ambient + diffuse) + spec);
                        if cap && let Some(hatch) = input.options.hatch {
                            let u = (x as f64 + y as f64) / ss as f64;
                            if u.rem_euclid(hatch.spacing_px) < hatch.width_px {
                                c = mix(c, to_linear(hatch.rgb), 0.55);
                            }
                        }
                        c
                    };
                    if input.options.outline {
                        let e = edge_strength(g, cam, x, y, depth_tol, mm_per_px);
                        if e > 0.0 {
                            c = mix(c, line_rgb, e);
                        }
                    }
                    for a in 0..3 {
                        acc[a] += c[a];
                    }
                }
            }
            let k = (ss * ss) as f64;
            let rgb = to_display(acc.map(|v| v / k));
            row[fx * 3..fx * 3 + 3].copy_from_slice(&rgb);
        }
    });
    out
}

#[allow(clippy::too_many_arguments)]
fn ambient_occlusion(
    g: &GBuffer,
    cam: &OrthoCamera,
    x: usize,
    y: usize,
    p: [f64; 3],
    n: [f64; 3],
    r_px: f64,
    r_mm: f64,
) -> f64 {
    let rot = BAYER4[(y % 4) * 4 + (x % 4)] / 16.0 * std::f64::consts::TAU;
    let golden = std::f64::consts::PI * (3.0 - 5.0_f64.sqrt());
    let mut occ = 0.0;
    for i in 0..AO_SAMPLES {
        let r = r_px * ((i as f64 + 0.5) / AO_SAMPLES as f64).sqrt();
        let a = rot + golden * i as f64;
        let qx = x as f64 + r * a.cos();
        let qy = y as f64 + r * a.sin();
        if qx < 0.0 || qy < 0.0 || qx >= g.width as f64 || qy >= g.height as f64 {
            continue;
        }
        let f = g.at(qx as usize, qy as usize);
        if f.layer == EMPTY {
            continue;
        }
        let q = cam.world(qx.floor() + 0.5, qy.floor() + 0.5, f64::from(f.depth));
        let v = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
        let d = length(v);
        if d < 1e-9 {
            continue;
        }
        let falloff = (1.0 - d / (1.5 * r_mm)).max(0.0);
        occ += (dot(n, v) / d - 0.12).max(0.0) * falloff;
    }
    (1.0 - 2.2 * occ / AO_SAMPLES as f64).clamp(0.25, 1.0)
}

fn edge_strength(g: &GBuffer, cam: &OrthoCamera, x: usize, y: usize, depth_tol: f64, mm_per_px: f64) -> f64 {
    let f = g.at(x, y);
    let mut e: f64 = 0.0;
    let nf = (f.layer != EMPTY).then(|| f.normal());
    for (dx, dy) in [(-1_i64, 0_i64), (1, 0), (0, -1), (0, 1)] {
        let (qx, qy) = (x as i64 + dx, y as i64 + dy);
        if qx < 0 || qy < 0 || qx >= g.width as i64 || qy >= g.height as i64 {
            continue;
        }
        let q = g.at(qx as usize, qy as usize);
        match (f.layer == EMPTY, q.layer == EMPTY) {
            (true, true) => {}
            (true, false) | (false, true) => e = e.max(0.9),
            (false, false) => {
                let (n, m) = (nf.unwrap_or([0.0, 0.0, 1.0]), q.normal());
                if (f.flags ^ q.flags) & FLAG_CAP != 0 {
                    e = e.max(0.8);
                } else if f.layer != q.layer {
                    e = e.max(0.55);
                }

                let nr = dot(n, cam.right);
                let nu = dot(n, cam.up);
                let nfw = dot(n, cam.forward);
                let nfw = if nfw.abs() < 0.2 { 0.2_f64.copysign(nfw) } else { nfw };
                let pred = -(nr * dx as f64 * mm_per_px - nu * dy as f64 * mm_per_px) / nfw;
                let step = (f64::from(q.depth) - f64::from(f.depth) - pred).abs();
                if step > depth_tol + 0.5 * pred.abs() {
                    e = e.max(0.85);
                } else if dot(n, m) < 0.5 {
                    e = e.max(0.45);
                }
            }
        }
    }
    e
}
