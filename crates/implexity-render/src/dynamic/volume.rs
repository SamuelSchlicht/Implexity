// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![allow(clippy::cast_possible_wrap)]

use implexity_mesh::grid::Field3;
use implexity_mesh::raster::RgbImage;

use super::colormap::Scale;
use super::draw::{Canvas, fmt_num, nice_ticks, text_width};
use super::grid::{GridField, Reduce};
use super::plane::Theme;
use super::solid::Triangles;
use crate::RenderError;
use crate::render3d::{
    CameraRequest, CapRequest, ClipPlane, RasterOptions, RenderMesh, VertexColouring, clip_mesh,
    extract_isosurface, grid_section_cap, rasterize, resolve_camera,
};

pub const MAX_TRIANGLES: usize = 1_500_000;
pub const MAX_BACKDROP_CELLS: usize = 192;

const GREY: [f64; 3] = [170.0, 176.0, 186.0];
const BAR: usize = 90;

#[derive(Clone, Copy, Debug)]
pub struct BodySurface<'a> {
    pub surface: &'a Triangles,
    pub cap: Option<&'a Triangles>,
    pub colour: Option<Scale>,
    pub label: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub struct Backdrop<'a> {
    pub field: &'a GridField,
    pub reduce: Reduce,
    pub scale: Scale,
    pub axis: usize,
    pub at: f64,
}

#[derive(Clone, Debug)]
pub struct VolumeView<'a> {
    pub surface: Option<&'a GridField>,
    pub iso: f64,
    pub colour: Option<(&'a GridField, Reduce, Scale)>,
    pub body: Option<BodySurface<'a>>,
    pub backdrop: Option<Backdrop<'a>>,
    pub clip: Option<([f64; 3], [f64; 3])>,
    pub camera: String,
    pub width: usize,
    pub height: usize,
    pub theme: Theme,
    pub title: (String, String),
    pub bar_label: String,
}

struct Parts {
    mesh: RenderMesh,
    colours: Vec<[f64; 3]>,
    lo: [f64; 3],
    hi: [f64; 3],
}

impl Parts {
    fn new() -> Self {
        Self {
            mesh: RenderMesh { vertices: Vec::new(), normals: Vec::new(), faces: Vec::new() },
            colours: Vec::new(),
            lo: [f64::INFINITY; 3],
            hi: [f64::NEG_INFINITY; 3],
        }
    }

    fn extend_bounds(&mut self, lo: [f64; 3], hi: [f64; 3]) {
        for a in 0..3 {
            self.lo[a] = self.lo[a].min(lo[a]);
            self.hi[a] = self.hi[a].max(hi[a]);
        }
    }

    fn add(&mut self, mesh: &RenderMesh, colours: &[[f64; 3]]) {
        let base = self.mesh.vertices.len();
        for v in &mesh.vertices {
            self.extend_bounds(*v, *v);
        }
        self.mesh.vertices.extend_from_slice(&mesh.vertices);
        self.mesh.normals.extend_from_slice(&mesh.normals);
        self.mesh.faces.extend(mesh.faces.iter().map(|f| f.map(|i| i + base)));
        self.colours.extend_from_slice(colours);
    }

    fn triangle(&mut self, p: [[f64; 3]; 3], c: [[f64; 3]; 3], normal: Option<[f64; 3]>) {
        let n = normal.unwrap_or_else(|| {
            let (a, b) = (sub(p[1], p[0]), sub(p[2], p[0]));
            let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if l > 0.0 { n.map(|x| x / l) } else { [0.0, 0.0, 1.0] }
        });
        let base = self.mesh.vertices.len();
        for (v, col) in p.iter().zip(c) {
            self.extend_bounds(*v, *v);
            self.mesh.vertices.push(*v);
            self.mesh.normals.push(n);
            self.colours.push(col);
        }
        self.mesh.faces.push([base, base + 1, base + 2]);
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn colour_of(scale: Option<Scale>, v: f64) -> [f64; 3] {
    match scale {
        Some(s) if v.is_finite() => s.colour(v).map(f64::from),
        _ => GREY,
    }
}

fn triangles_mesh(t: &Triangles) -> (RenderMesh, Vec<f64>) {
    let mut p = Parts::new();
    for tri in &t.points {
        p.triangle(*tri, [[0.0; 3]; 3], None);
    }
    (p.mesh, t.values.iter().flatten().copied().collect())
}

fn bar(canvas: &mut Canvas, x: i64, h3: usize, scale: &Scale, label: &str, fg: [u8; 3]) {
    let (top, h) = (48_i64, (h3 as i64 - 40).max(20));
    for r in 0..h {
        canvas.rect(x, top + r, 14, 1, scale.map.map(1.0 - r as f64 / (h - 1).max(1) as f64));
    }
    for tick in nice_ticks(scale.lo, scale.hi, 5) {
        let t = (tick - scale.lo) / (scale.hi - scale.lo);
        #[allow(clippy::cast_possible_truncation)]
        let y = top + ((1.0 - t) * (h - 1) as f64).round() as i64;
        canvas.rect(x + 14, y, 4, 1, fg);
        canvas.text(x + 21, y - 3, &fmt_num(tick), fg, 1);
    }
    let lw = i64::try_from(text_width(label, 1)).unwrap_or(0);
    canvas.text((x + 7 - lw / 2).max(x - 10), top - 12, label, fg, 1);
}



#[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
pub fn render_volume(v: &VolumeView<'_>) -> Result<RgbImage, RenderError> {
    let clip = v.clip.map(|(point, normal)| {
        let n = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt().max(1e-300);
        ClipPlane {
            point_mm: point,
            normal: normal.map(|x| x / n),
            keep_negative: true,
            cap: Some(CapRequest { inside: "above_iso".into(), complement_color_rgb: None }),
        }
    });
    let mut parts = Parts::new();

    let iso_field = match v.surface {
        Some(s) => {
            if s.dims() != 3 || s.components != 1 {
                return Err(RenderError::Invalid("an iso-surface needs a 3-D scalar field".into()));
            }
            let shape = [s.shape[0], s.shape[1], s.shape[2]];
            if shape.iter().any(|&n| n < 2) {
                return Err(RenderError::Invalid("an iso-surface needs at least 2 cells per axis".into()));
            }
            let bbox: [[f64; 3]; 2] = [
                std::array::from_fn(|a| s.origin[a]),
                std::array::from_fn(|a| s.origin[a] + (shape[a] as f64 - 1.0) * s.spacing[a]),
            ];
            Some((Field3::new(shape, &s.values)?, bbox))
        }
        None => None,
    };
    let sample_colour = |p: &[f64; 3]| -> Option<f64> {
        let (f, r, _) = v.colour?;
        f.sample_scalar(p, r)
    };
    let mut cap = None;
    if let Some((field, bbox)) = &iso_field {
        let (mesh, _) = extract_isosurface(field, *bbox, v.iso, MAX_TRIANGLES)?;
        let values: Option<Vec<f64>> =
            v.colour.map(|_| mesh.vertices.iter().map(|p| sample_colour(p).unwrap_or(f64::NAN)).collect());
        let (mesh, values) = clip_mesh(&mesh, values.as_deref(), clip.as_ref(), MAX_TRIANGLES)?;
        let colours: Vec<[f64; 3]> = match (&values, v.colour) {
            (Some(vals), Some((_, _, scale))) => {
                vals.iter().map(|x| scale.colour(*x).map(f64::from)).collect()
            }
            _ => vec![crate::render3d::BASE_COLOUR; mesh.vertices.len()],
        };
        parts.add(&mesh, &colours);
        parts.extend_bounds(bbox[0], bbox[1]);
        let cap_colour = |points: &[[f64; 3]]| -> Result<(Vec<[f64; 3]>, Vec<bool>), RenderError> {
            let mut cols = Vec::with_capacity(points.len());
            let mut ok = Vec::with_capacity(points.len());
            for p in points {
                match (v.colour, sample_colour(p)) {
                    (Some((_, _, scale)), Some(x)) => {
                        cols.push(scale.colour(x).map(f64::from));
                        ok.push(true);
                    }
                    (None, _) => {
                        cols.push(crate::render3d::BASE_COLOUR);
                        ok.push(true);
                    }
                    _ => {
                        cols.push([128.0; 3]);
                        ok.push(false);
                    }
                }
            }
            Ok((cols, ok))
        };
        if let Some(c) = &clip {
            cap = Some(grid_section_cap(
                field,
                *bbox,
                v.iso,
                "above_iso",
                *bbox,
                c,
                Box::new(cap_colour),
                None,
            )?);
        }
    }

    if let Some(b) = &v.body {
        let (mesh, values) = triangles_mesh(b.surface);
        let keeps_some = clip.as_ref().is_none_or(|c| {
            mesh.vertices.iter().any(|p| {
                (p[0] - c.point_mm[0]) * c.normal[0]
                    + (p[1] - c.point_mm[1]) * c.normal[1]
                    + (p[2] - c.point_mm[2]) * c.normal[2]
                    <= 0.0
            })
        });
        if !mesh.faces.is_empty() && keeps_some {
            let (mesh, values) = clip_mesh(&mesh, Some(&values), clip.as_ref(), MAX_TRIANGLES)?;
            let colours: Vec<[f64; 3]> =
                values.unwrap_or_default().iter().map(|x| colour_of(b.colour, *x)).collect();
            parts.add(&mesh, &colours);
        }
        if let (Some(t), Some(c)) = (b.cap, &clip) {
            for (tri, vals) in t.points.iter().zip(&t.values) {
                parts.triangle(*tri, vals.map(|x| colour_of(b.colour, x)), Some(c.normal));
            }
        }
    }

    let around = v.body.and_then(|b| b.surface.bounds()).map(|(lo, hi)| {
        let ext = 0.5 * (0..3).map(|a| hi[a] - lo[a]).fold(0.0_f64, f64::max);
        (lo.map(|x| x - ext), hi.map(|x| x + ext))
    });
    if let Some(bd) = &v.backdrop {
        let f = bd.field;
        if f.dims() != 2 || bd.axis > 2 {
            return Err(RenderError::Invalid("a backdrop is a 2-D slice with a normal axis".into()));
        }
        let keep: Vec<usize> = (0..3).filter(|&a| a != bd.axis).collect();
        let (nu, nv) = (f.shape[0], f.shape[1]);
        let (su, sv) = (nu.div_ceil(MAX_BACKDROP_CELLS).max(1), nv.div_ceil(MAX_BACKDROP_CELLS).max(1));
        let mut normal = [0.0; 3];
        normal[bd.axis] = 1.0;
        let point = |u: f64, w: f64| {
            let mut p = [0.0; 3];
            p[keep[0]] = u;
            p[keep[1]] = w;
            p[bd.axis] = bd.at;
            p
        };
        for i in (0..nu).step_by(su) {
            for j in (0..nv).step_by(sv) {
                let (du, dv) = (f.spacing[0] * su.min(nu - i) as f64, f.spacing[1] * sv.min(nv - j) as f64);
                let u0 = f.origin[0] + (i as f64 - 0.5) * f.spacing[0];
                let v0 = f.origin[1] + (j as f64 - 0.5) * f.spacing[1];
                let centre = [u0 + 0.5 * du, v0 + 0.5 * dv];
                if let Some((lo, hi)) = &around {
                    let (a, b) = (keep[0], keep[1]);
                    if centre[0] < lo[a] || centre[0] > hi[a] || centre[1] < lo[b] || centre[1] > hi[b] {
                        continue;
                    }
                }
                let Some(x) = f.sample_scalar(&centre, bd.reduce) else { continue };
                let c = bd.scale.colour(x).map(f64::from);
                let (p00, p10, p11, p01) =
                    (point(u0, v0), point(u0 + du, v0), point(u0 + du, v0 + dv), point(u0, v0 + dv));
                parts.triangle([p00, p10, p11], [c; 3], Some(normal));
                parts.triangle([p00, p11, p01], [c; 3], Some(normal));
            }
        }
    }
    if parts.mesh.faces.is_empty() {
        return Err(RenderError::Invalid(
            "the 3-D frame has nothing to draw (everything is clipped away)".into(),
        ));
    }
    let main_scale = v.colour.map(|c| c.2).or(v.backdrop.map(|b| b.scale));
    let body_scale = v.body.and_then(|b| b.colour.map(|s| (s, b.label)));
    let bars = usize::from(main_scale.is_some()) + usize::from(body_scale.is_some());
    let (w3, h3) = (v.width.saturating_sub(bars * BAR).max(64), v.height.saturating_sub(30).max(64));

    let bbox = if let (Some((_, grid_box)), true) = (&iso_field, v.body.is_none() && v.backdrop.is_none()) {
        *grid_box
    } else {
        let pad: Vec<f64> = (0..3).map(|a| 0.02 * (parts.hi[a] - parts.lo[a]).abs().max(1e-12)).collect();
        [std::array::from_fn(|a| parts.lo[a] - pad[a]), std::array::from_fn(|a| parts.hi[a] + pad[a])]
    };
    let camera =
        resolve_camera(&CameraRequest::Preset { preset: v.camera.clone(), fov_deg: 30.0 }, bbox, w3, h3)?;
    let uniform = v.colour.is_none() && v.body.is_none() && v.backdrop.is_none();
    let colouring = if uniform { VertexColouring::Uniform } else { VertexColouring::Colours(&parts.colours) };
    let background = match v.theme {
        Theme::Dark => "dark",
        Theme::White => "white",
    };
    let (img, _) = rasterize(
        &parts.mesh,
        &RasterOptions {
            width: w3,
            height: h3,
            camera: &camera,
            colouring,
            background,
            supersample: 2,
            base_colour: None,
            cap: cap.as_ref(),
        },
    )?;
    let mut canvas = Canvas::new(w3 + bars * BAR, h3 + 30, v.theme.background());
    canvas.blit(&img, 0, 30);
    let fg = v.theme.text();
    canvas.text(6, 5, &v.title.0, fg, 1);
    canvas.text(6, 16, &v.title.1, fg, 1);
    let mut x = (w3 + 14) as i64;
    if let Some(scale) = main_scale {
        bar(&mut canvas, x, h3, &scale, &v.bar_label, fg);
        x += BAR as i64;
    }
    if let Some((scale, label)) = body_scale {
        bar(&mut canvas, x, h3, &scale, label, fg);
    }
    Ok(canvas.img)
}

