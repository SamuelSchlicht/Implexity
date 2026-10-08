// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Map, Value, json};

use crate::MeshError;
use crate::grid::{Field3, Grid3};
use crate::mc::{GradientDirection, marching_cubes};
use crate::numeric::{pairwise_sum, percentile, py_round_digits};
use crate::pyfmt::{fmt_f, fmt_g};
use crate::topology::{self, Tri, Vec3, areas, orientation_report, signed_volume, topology_fast, weld_exact};

pub const MM: f64 = 1.0e-3;
pub const PAD_CELLS: usize = 2;
pub const GRID_OFFSET_FRAC: f64 = 0.5;
pub const DEADBAND_FRAC: f64 = 1e-4;
pub const BAND_CELLS: f64 = 6.0;
pub const EXP_CLAMP: (f64, f64) = (1.0, 2.5);

fn env_f64(name: &str, default: &str) -> f64 {
    let raw = std::env::var(name).unwrap_or_else(|_| default.to_string());
    raw.trim().parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or_else(|| default.parse().unwrap_or(0.0))
}

#[must_use]
pub fn max_samples() -> usize {
    crate::cast::trunc_usize(env_f64("IMPLEXITY_MAX_BODY_SAMPLES", "48e6").max(0.0))
}

#[must_use]
pub fn slab_cells() -> usize {
    std::env::var("IMPLEXITY_BODY_SLAB")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .map_or(24, |v| usize::try_from(v).unwrap_or(0))
}

#[must_use]
pub fn calibrate_div() -> f64 {
    env_f64("IMPLEXITY_BODY_CALIBRATE_DIV", "4")
}

#[derive(Clone)]
pub struct DesignSnapshot {
    pub version: i64,
    pub meta: Map<String, Value>,
    pub params: Arc<dyn Any + Send + Sync>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FastFields {
    pub f: Vec<f64>,
    pub nu: Vec<f64>,
    pub tau: Vec<f64>,
    pub mtilde: Vec<f64>,
}

pub type GeffCache = Mutex<Option<(i64, Arc<Grid3>)>>;

pub trait BodyEvaluator: Sync {
    fn h_design(&self) -> f64;
    fn blur_radius_design(&self) -> i64;
    fn interface_eps(&self) -> f64;
    fn domain(&self) -> [f64; 3];
    fn snapshot(&self) -> DesignSnapshot;
    fn bind_continuation(&self, snap: &DesignSnapshot);
    fn prep_box(&self, origin: Vec3, shape: [usize; 3], h: f64) -> [Vec<f64>; 3] {
        let n = shape[0] * shape[1] * shape[2];
        let mut out: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::with_capacity(n));
        let eps = 0.5 * self.h_design();
        let dom = self.domain();
        for i in 0..shape[0] {
            let xi = i as f64 * h;
            for j in 0..shape[1] {
                let yj = j as f64 * h;
                for k in 0..shape[2] {
                    let zk = k as f64 * h;
                    let p = [
                        origin[0] + xi + yj * 0.0 + zk * 0.0,
                        origin[1] + xi * 0.0 + yj + zk * 0.0,
                        origin[2] + xi * 0.0 + yj * 0.0 + zk,
                    ];
                    for c in 0..3 {
                        out[c].push(crate::numeric::clip(p[c], eps, dom[c] - eps));
                    }
                }
            }
        }
        out
    }

    fn run_fast(&self, snap: &DesignSnapshot, pts: &[Vec<f64>; 3], h: f64) -> Result<FastFields, MeshError>;

    fn run_geff(
        &self,
        snap: &DesignSnapshot,
        pts: &[Vec<f64>; 3],
        shape: [usize; 3],
        h: f64,
        r: i64,
    ) -> Result<Vec<f64>, MeshError>;
    fn geff_cache(&self) -> &GeffCache;
}

pub trait SdfKernel: Sync {

    fn winding_number(&self, v: &[Vec3], f: &[Tri], p: &[Vec3]) -> Result<Vec<f64>, MeshError>;

    fn unsigned_distance(
        &self,
        v: &[Vec3],
        f: &[Tri],
        p: &[Vec3],
    ) -> Result<(Vec<f64>, Vec<usize>), MeshError>;

    fn sdf_at(&self, v: &[Vec3], f: &[Tri], p: &[Vec3]) -> Result<Vec<f64>, MeshError>;
    fn mesh_volume(&self, v: &[Vec3], f: &[Tri]) -> f64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeSdf;

fn trimesh(v: &[Vec3], f: &[Tri]) -> implexity_geometry::domain_sdf::TriMesh {
    implexity_geometry::domain_sdf::TriMesh { v: v.to_vec(), f: f.to_vec() }
}

impl SdfKernel for NativeSdf {
    fn winding_number(&self, v: &[Vec3], f: &[Tri], p: &[Vec3]) -> Result<Vec<f64>, MeshError> {
        Ok(implexity_geometry::domain_sdf::winding_number(&trimesh(v, f), p))
    }
    fn unsigned_distance(
        &self,
        v: &[Vec3],
        f: &[Tri],
        p: &[Vec3],
    ) -> Result<(Vec<f64>, Vec<usize>), MeshError> {
        Ok(implexity_geometry::domain_sdf::unsigned_distance(&trimesh(v, f), p))
    }
    fn sdf_at(&self, v: &[Vec3], f: &[Tri], p: &[Vec3]) -> Result<Vec<f64>, MeshError> {
        Ok(implexity_geometry::domain_sdf::sdf_at(&trimesh(v, f), p).0)
    }
    fn mesh_volume(&self, v: &[Vec3], f: &[Tri]) -> f64 {
        implexity_geometry::domain_sdf::mesh_volume(&trimesh(v, f))
    }
}

type Cancel<'a> = Option<&'a (dyn Fn() -> bool + Sync)>;
type Progress<'a> = Option<&'a (dyn Fn(f64) + Sync)>;

fn check_cancel(cancel: Cancel<'_>) -> Result<(), MeshError> {
    if cancel.is_some_and(|c| c()) { Err(MeshError::Cancelled) } else { Ok(()) }
}

fn axis_coords(origin: Vec3, h: f64, shape: [usize; 3], a: usize) -> Vec<f64> {
    (0..shape[a]).map(|i| origin[a] + i as f64 * h).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoxCap {
    pub lo: Vec3,
    pub hi: Vec3,
}

impl BoxCap {
    fn describe(&self) -> Value {
        json!({"kind": "box", "lo_mm": self.lo.iter().map(|v| v * 1e3).collect::<Vec<_>>(),
               "hi_mm": self.hi.iter().map(|v| v * 1e3).collect::<Vec<_>>(),
               "exact": "analytic box signed distance, exact everywhere"})
    }

    fn volume_m3(&self) -> f64 {
        (self.hi[0] - self.lo[0]) * (self.hi[1] - self.lo[1]) * (self.hi[2] - self.lo[2])
    }

    #[must_use]
    pub fn phi_grid(&self, origin: Vec3, h: f64, shape: [usize; 3]) -> Vec<f32> {
        let qs: [Vec<f64>; 3] = std::array::from_fn(|a| {
            let c = 0.5 * (self.lo[a] + self.hi[a]);
            let b = 0.5 * (self.hi[a] - self.lo[a]);
            axis_coords(origin, h, shape, a).iter().map(|x| (x - c).abs() - b).collect()
        });
        let mut out = Vec::with_capacity(shape.iter().product());
        for &q0 in &qs[0] {
            for &q1 in &qs[1] {
                for &q2 in &qs[2] {
                    let qmax = q0.max(q1).max(q2);
                    let (a, b, c) = (q0.max(0.0), q1.max(0.0), q2.max(0.0));
                    let outside = a * a + b * b + c * c;
                    out.push(crate::cast::f32_of(outside.sqrt() + qmax.min(0.0)));
                }
            }
        }
        out
    }

    #[must_use]
    pub fn occupancy(&self, origin: Vec3, h: f64, shape: [usize; 3]) -> Vec<f32> {
        let f: [Vec<f64>; 3] = std::array::from_fn(|a| {
            axis_coords(origin, h, shape, a)
                .iter()
                .map(|x| {
                    let lo = (x - 0.5 * h).max(self.lo[a]);
                    let hi = (x + 0.5 * h).min(self.hi[a]);
                    crate::numeric::clip((hi - lo) / h, 0.0, 1.0)
                })
                .collect()
        });
        let mut out = Vec::with_capacity(shape.iter().product());
        for &a in &f[0] {
            for &b in &f[1] {
                for &c in &f[2] {
                    out.push(crate::cast::f32_of(a * b * c));
                }
            }
        }
        out
    }
}

pub struct MeshCap<'a> {
    pub v: Vec<Vec3>,
    pub f: Vec<Tri>,
    pub name: String,
    pub kernel: &'a dyn SdfKernel,
    stats: Mutex<Map<String, Value>>,
    band: Mutex<Option<Vec<bool>>>,
}

impl<'a> MeshCap<'a> {
    #[must_use]
    pub fn new(v: Vec<Vec3>, f: Vec<Tri>, name: &str, kernel: &'a dyn SdfKernel) -> Self {
        Self { v, f, name: name.into(), kernel, stats: Mutex::new(Map::new()), band: Mutex::new(None) }
    }


    pub fn from_file(path: &Path, units_mm: bool, kernel: &'a dyn SdfKernel) -> Result<Self, MeshError> {
        let data =
            std::fs::read(path).map_err(|e| MeshError::io(format!("reading {}", path.display()), e))?;
        let name = path.file_name().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let m = implexity_geometry::domain_sdf::read_mesh(&data, &name, if units_mm { 1e-3 } else { 1.0 })
            .map_err(|e| MeshError::Rejected(e.problems))?;
        Ok(Self::new(m.v, m.f, &name, kernel))
    }

    fn bounds(&self) -> (Vec3, Vec3) {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in &self.v {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        (lo, hi)
    }

    fn describe(&self) -> Value {
        let (lo, hi) = self.bounds();
        let mut d = Map::new();
        d.insert("kind".into(), json!("mesh"));
        d.insert("name".into(), json!(self.name));
        d.insert("triangles".into(), json!(self.f.len()));
        d.insert("vertices".into(), json!(self.v.len()));
        d.insert("bbox_mm".into(), json!([lo.map(|v| v * 1e3), hi.map(|v| v * 1e3)]));
        d.insert(
            "exact".into(),
            json!("winding-number sign at every node; exact unsigned distance on the sign-change band"),
        );
        for (k, v) in lock(&self.stats).iter() {
            d.insert(k.clone(), v.clone());
        }
        Value::Object(d)
    }

    fn volume_m3(&self) -> f64 {
        self.kernel.mesh_volume(&self.v, &self.f)
    }


    pub fn phi_grid(
        &self,
        origin: Vec3,
        h: f64,
        shape: [usize; 3],
        cancel: Cancel<'_>,
    ) -> Result<Vec<f32>, MeshError> {
        let [nx, ny, nz] = shape;
        let ax: [Vec<f64>; 3] = std::array::from_fn(|a| axis_coords(origin, h, shape, a));
        let t0 = Instant::now();
        let mut inside = vec![false; nx * ny * nz];
        let (vlo, vhi) = self.bounds();
        let lo = vlo.map(|v| v - 2.0 * h);
        let hi = vhi.map(|v| v + 2.0 * h);
        let step = (2_000_000 / (nx * ny).max(1)).max(1);
        let mut z0 = 0;
        while z0 < nz {
            check_cancel(cancel)?;
            let z1 = (z0 + step).min(nz);
            let zs = &ax[2][z0..z1];
            let zmax = zs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let zmin = zs.iter().copied().fold(f64::INFINITY, f64::min);
            if zmax < lo[2] || zmin > hi[2] {
                z0 = z1;
                continue;
            }
            let mut pts = Vec::new();
            let mut where_ = Vec::new();
            for (i, &x) in ax[0].iter().enumerate() {
                for (j, &y) in ax[1].iter().enumerate() {
                    for (kk, &z) in zs.iter().enumerate() {
                        let p = [x, y, z];
                        if (0..3).all(|a| p[a] >= lo[a] && p[a] <= hi[a]) {
                            pts.push(p);
                            where_.push((i * ny + j) * nz + z0 + kk);
                        }
                    }
                }
            }
            if !pts.is_empty() {
                let w = self.kernel.winding_number(&self.v, &self.f, &pts)?;
                for (n, &at) in where_.iter().enumerate() {
                    inside[at] = w[n] > 0.5;
                }
            }
            z0 = z1;
        }
        let t_sign = t0.elapsed().as_secs_f64();
        let mut band = vec![false; nx * ny * nz];
        let strides = [ny * nz, nz, 1];
        for a in 0..3 {
            for i in 0..nx {
                for j in 0..ny {
                    for k in 0..nz {
                        let ijk = [i, j, k];
                        if ijk[a] + 1 >= shape[a] {
                            continue;
                        }
                        let at = (i * ny + j) * nz + k;
                        if inside[at] != inside[at + strides[a]] {
                            band[at] = true;
                            band[at + strides[a]] = true;
                        }
                    }
                }
            }
        }
        let t0 = Instant::now();
        let big = BAND_CELLS * h;
        let mut dist = vec![big; nx * ny * nz];
        let idx: Vec<usize> = (0..band.len()).filter(|&i| band[i]).collect();
        if !idx.is_empty() {
            let pts: Vec<Vec3> =
                idx.iter().map(|&n| [ax[0][n / (ny * nz)], ax[1][(n / nz) % ny], ax[2][n % nz]]).collect();
            let (d, _tri) = self.kernel.unsigned_distance(&self.v, &self.f, &pts)?;
            for (m, &n) in idx.iter().enumerate() {
                dist[n] = d[m].min(big);
            }
        }
        let t_dist = t0.elapsed().as_secs_f64();
        *lock(&self.stats) = [
            ("sign_nodes", json!(nx * ny * nz)),
            ("band_nodes", json!(idx.len())),
            ("sign_seconds", json!(py_round_digits(t_sign, 2))),
            ("distance_seconds", json!(py_round_digits(t_dist, 2))),
            ("band_cells", json!(BAND_CELLS)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        *lock(&self.band) = Some(band);
        Ok(inside.iter().zip(&dist).map(|(&ins, &d)| crate::cast::f32_of(if ins { -d } else { d })).collect())
    }

    #[must_use]
    pub fn occupancy(phid: &[f32], h: f64) -> Vec<f32> {
        let w = h.max(1e-300);
        phid.iter()
            .map(|&s| crate::cast::f32_of(crate::numeric::clip(0.5 - f64::from(s) / w, 0.0, 1.0)))
            .collect()
    }


    pub fn verify(
        &self,
        origin: Vec3,
        h: f64,
        shape: [usize; 3],
        phi: &[f32],
        n_sample: usize,
        seed: u128,
    ) -> Result<Value, MeshError> {
        let mut rng = implexity_core::rng::default_rng(seed);
        let n: usize = shape.iter().product();
        let k = n_sample.min(n);
        let pick = rng.choice_without_replacement(n, k).map_err(|e| MeshError::invalid(e.to_string()))?;
        let at = |p: usize| -> Vec3 {
            let ijk = [p / (shape[1] * shape[2]), (p / shape[2]) % shape[1], p % shape[2]];
            std::array::from_fn(|a| origin[a] + ijk[a] as f64 * h)
        };
        let pick: Vec<usize> = pick.iter().map(|&p| crate::cast::idx(p)).collect();
        let pts: Vec<Vec3> = pick.iter().map(|&p| at(p)).collect();
        let d_true = self.kernel.sdf_at(&self.v, &self.f, &pts)?;
        let sign = |x: f64| {
            if x > 0.0 {
                1
            } else if x < 0.0 {
                -1
            } else {
                0
            }
        };
        let sign_bad =
            pick.iter().zip(&d_true).filter(|(p, d)| sign(**d) != sign(f64::from(phi[**p]))).count();
        let mut out = Map::new();
        out.insert("sign_sampled".into(), json!(k));
        out.insert("sign_mismatches".into(), json!(sign_bad));
        let band = lock(&self.band).clone();
        if let Some(band) = band
            && band.iter().any(|&b| b)
        {
            let bidx: Vec<usize> = (0..band.len()).filter(|&i| band[i]).collect();
            let kb = n_sample.min(bidx.len());
            let sel = rng
                .choice_without_replacement(bidx.len(), kb)
                .map_err(|e| MeshError::invalid(e.to_string()))?;
            let pb: Vec<usize> = sel.iter().map(|&s| bidx[crate::cast::idx(s)]).collect();
            let ptsb: Vec<Vec3> = pb.iter().map(|&p| at(p)).collect();
            let db = self.kernel.sdf_at(&self.v, &self.f, &ptsb)?;
            let err = pb
                .iter()
                .zip(&db)
                .map(|(p, d)| (d - f64::from(phi[*p])).abs())
                .fold(f64::NEG_INFINITY, f64::max);
            let bad = pb.iter().zip(&db).filter(|(p, d)| sign(**d) != sign(f64::from(phi[**p]))).count();
            out.insert("band_sampled".into(), json!(kb));
            out.insert("band_max_abs_distance_error_mm".into(), json!(err * 1e3));
            out.insert("band_sign_mismatches".into(), json!(bad));
        }
        Ok(Value::Object(out))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub enum Cap<'a> {
    Box(BoxCap),
    Mesh(MeshCap<'a>),
}

impl Cap<'_> {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Box(_) => "box",
            Self::Mesh(_) => "mesh",
        }
    }
    fn describe(&self) -> Value {
        match self {
            Self::Box(b) => b.describe(),
            Self::Mesh(m) => m.describe(),
        }
    }
    fn volume_m3(&self) -> f64 {
        match self {
            Self::Box(b) => b.volume_m3(),
            Self::Mesh(m) => m.volume_m3(),
        }
    }
    fn bounds(&self) -> (Vec3, Vec3) {
        match self {
            Self::Box(b) => (b.lo, b.hi),
            Self::Mesh(m) => m.bounds(),
        }
    }
}

#[must_use]
pub fn grid_for(cap: &Cap<'_>, h: f64, pad: usize) -> (Vec3, [usize; 3]) {
    let (lo, hi) = cap.bounds();
    let origin = lo.map(|v| v - (pad as f64 - GRID_OFFSET_FRAC) * h);
    let n = std::array::from_fn(|a| crate::cast::trunc_usize(((hi[a] - lo[a]) / h).ceil()) + 2 * pad + 1);
    (origin, n)
}

#[must_use]
pub fn trilerp(f: &Grid3, ix: f64, iy: f64, iz: f64) -> f64 {
    let [nx, ny, nz] = f.shape;
    let fl = |v: f64, n: usize| crate::numeric::clip(v.floor(), 0.0, (n as f64) - 2.0);
    let (x0f, y0f, z0f) = (fl(ix, nx), fl(iy, ny), fl(iz, nz));
    let (x0, y0, z0) = (x0f as usize, y0f as usize, z0f as usize);
    let fx = crate::numeric::clip(ix - x0f, 0.0, 1.0);
    let fy = crate::numeric::clip(iy - y0f, 0.0, 1.0);
    let fz = crate::numeric::clip(iz - z0f, 0.0, 1.0);
    let (x1, y1, z1) = (x0 + 1, y0 + 1, z0 + 1);
    let g = |i: usize, j: usize, k: usize| f.data[(i * ny + j) * nz + k];
    let c00 = g(x0, y0, z0) * (1.0 - fx) + g(x1, y0, z0) * fx;
    let c01 = g(x0, y0, z1) * (1.0 - fx) + g(x1, y0, z1) * fx;
    let c10 = g(x0, y1, z0) * (1.0 - fx) + g(x1, y1, z0) * fx;
    let c11 = g(x0, y1, z1) * (1.0 - fx) + g(x1, y1, z1) * fx;
    (c00 * (1.0 - fy) + c10 * fy) * (1.0 - fz) + (c01 * (1.0 - fy) + c11 * fy) * fz
}

fn meta_f64(meta: &Map<String, Value>, key: &str) -> Result<f64, MeshError> {
    meta.get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| MeshError::invalid(format!("design meta lacks {key}")))
}


pub fn design_geff(ev: &dyn BodyEvaluator, snap: &DesignSnapshot) -> Result<Arc<Grid3>, MeshError> {
    if let Some((v, g)) = lock(ev.geff_cache()).as_ref()
        && *v == snap.version
    {
        return Ok(Arc::clone(g));
    }
    let dg: Vec<usize> = snap
        .meta
        .get("design_grid")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_f64).map(crate::cast::trunc_usize).collect())
        .filter(|v: &Vec<usize>| v.len() == 3)
        .ok_or_else(|| MeshError::invalid("design meta lacks design_grid"))?;
    let hd = ev.h_design();
    let r = ev.blur_radius_design();
    let pad = usize::try_from(r + 2).unwrap_or(2);
    let n = [dg[0] + 2 * pad, dg[1] + 2 * pad, dg[2] + 2 * pad];
    let o = (0.5 - pad as f64) * hd;
    let pts = ev.prep_box([o, o, o], n, hd);
    let res = ev.run_geff(snap, &pts, n, hd, r)?;
    let mut g = Vec::with_capacity(dg[0] * dg[1] * dg[2]);
    for i in pad..pad + dg[0] {
        for j in pad..pad + dg[1] {
            for k in pad..pad + dg[2] {
                g.push(res[(i * n[1] + j) * n[2] + k]);
            }
        }
    }
    let grid = Arc::new(Grid3 { shape: [dg[0], dg[1], dg[2]], data: g });
    *lock(ev.geff_cache()) = Some((snap.version, Arc::clone(&grid)));
    Ok(grid)
}

fn rho_from_fast(ev: &dyn BodyEvaluator, fast: &FastFields, geff: &[f64], w: f64) -> (Vec<f64>, Vec<f64>) {
    let e2 = ev.interface_eps() * ev.interface_eps();
    let n = fast.f.len();
    let mut rho = Vec::with_capacity(n);
    let mut gg = Vec::with_capacity(n);
    for (i, &ge) in geff.iter().enumerate().take(n) {
        let (f, nu, tau, mt) = (fast.f[i], fast.nu[i], fast.tau[i], fast.mtilde[i]);
        let folded = (f * f + e2).sqrt();
        let q = ((1.0 - nu) * folded + nu * f) / ge.max(1e-300);
        let g = q - tau;
        let rho_lat = 0.5 * ((-0.5 * g / w).tanh() + 1.0);
        rho.push(mt * rho_lat);
        gg.push(g);
    }
    (rho, gg)
}

struct Fields {
    phi: Vec<f32>,
    dens: f64,
    lset: f64,
}

#[allow(clippy::too_many_arguments)]
fn eval_fields(
    ev: &dyn BodyEvaluator,
    origin: Vec3,
    h: f64,
    shape: [usize; 3],
    occ: &[f64],
    cell: f64,
    slab: usize,
    cancel: Cancel<'_>,
    progress: Option<&dyn Fn(f64)>,
) -> Result<Fields, MeshError> {
    let [nx, ny, nz] = shape;
    let snap = ev.snapshot();
    let hd = ev.h_design();
    let w = meta_f64(&snap.meta, "interface_w")? * hd;
    let gd = design_geff(ev, &snap)?;
    ev.bind_continuation(&snap);
    let mut phi = vec![0f32; nx * ny * nz];
    let (mut dens, mut lset) = (0.0, 0.0);
    let step = slab.max(1);
    let mut z0 = 0;
    while z0 < nz {
        check_cancel(cancel)?;
        let z1 = (z0 + step).min(nz);
        let nzs = z1 - z0;
        let o = [origin[0], origin[1], origin[2] + z0 as f64 * h];
        let pts = ev.prep_box(o, [nx, ny, nzs], h);
        let fast = ev.run_fast(&snap, &pts, h)?;
        let ge: Vec<f64> = (0..pts[0].len())
            .map(|i| trilerp(&gd, pts[0][i] / hd - 0.5, pts[1][i] / hd - 0.5, pts[2][i] / hd - 0.5))
            .collect();
        let (mut rho, _g) = rho_from_fast(ev, &fast, &ge, w);
        for r in &mut rho {
            *r = crate::numeric::clip(*r, 1e-300, 1.0);
        }
        let mut dv = Vec::with_capacity(rho.len());
        let mut lv = Vec::with_capacity(rho.len());
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nzs {
                    let s = (i * ny + j) * nzs + k;
                    let at = (i * ny + j) * nz + z0 + k;
                    dv.push(rho[s] * occ[at]);
                    lv.push(if rho[s] >= 0.5 { occ[at] } else { 0.0 });
                    phi[at] = crate::cast::f32_of(2.0 * w * (-(2.0 * rho[s]).ln()));
                }
            }
        }
        dens += pairwise_sum(&dv);
        lset += pairwise_sum(&lv);
        if let Some(p) = progress {
            p(z1 as f64 / nz as f64);
        }
        z0 = z1;
    }
    Ok(Fields { phi, dens: dens * cell, lset: lset * cell })
}

struct Exact {
    g: Vec<f64>,
    mtilde: Vec<f64>,
    phi_lat: Vec<f64>,
}

fn sample_exact(ev: &dyn BodyEvaluator, pts: &[Vec3], h: f64) -> Result<Exact, MeshError> {
    let snap = ev.snapshot();
    let hd = ev.h_design();
    let eps = 0.5 * hd;
    let dom = ev.domain();
    let p: [Vec<f64>; 3] =
        std::array::from_fn(|c| pts.iter().map(|q| crate::numeric::clip(q[c], eps, dom[c] - eps)).collect());
    ev.bind_continuation(&snap);
    let fast = ev.run_fast(&snap, &p, h)?;
    let gd = design_geff(ev, &snap)?;
    let ge: Vec<f64> = (0..pts.len())
        .map(|i| trilerp(&gd, p[0][i] / hd - 0.5, p[1][i] / hd - 0.5, p[2][i] / hd - 0.5))
        .collect();
    let w = meta_f64(&snap.meta, "interface_w")? * hd;
    let (rho, g) = rho_from_fast(ev, &fast, &ge, w);
    let phi_lat =
        rho.iter().map(|&r| 2.0 * w * (-(2.0 * crate::numeric::clip(r, 1e-300, 1.0)).ln())).collect();
    Ok(Exact { g, mtilde: fast.mtilde, phi_lat })
}

#[must_use]
pub fn snap_spacing(ev: &dyn BodyEvaluator, h_mm: f64) -> f64 {
    let hd = ev.h_design() * 1e3;
    let k = crate::numeric::py_round(hd / h_mm.max(1e-9)).max(1.0);
    hd / k
}

fn deplateau(phi: &mut [f32], eps: f32) -> usize {
    let mut n = 0;
    for v in phi.iter_mut() {
        if v.abs() < eps {
            *v = if *v < 0.0 { -eps } else { eps };
            n += 1;
        }
    }
    n
}

fn mc_slabs(phi: &Grid3F32, slab: usize, cancel: Cancel<'_>) -> Result<(Vec<[f32; 3]>, Vec<Tri>), MeshError> {
    mc_slabs_of(&phi.data, phi.shape, slab, cancel)
}


pub fn mc_slabs_f64(
    phi: &[f64],
    shape: [usize; 3],
    slab: usize,
    cancel: Cancel<'_>,
) -> Result<(Vec<[f32; 3]>, Vec<Tri>), MeshError> {
    mc_slabs_of(phi, shape, slab, cancel)
}

fn mc_slabs_of<T: Copy + Into<f64>>(
    data: &[T],
    shape: [usize; 3],
    slab: usize,
    cancel: Cancel<'_>,
) -> Result<(Vec<[f32; 3]>, Vec<Tri>), MeshError> {
    let [nx, ny, nz] = shape;
    let (mut vs, mut fs) = (Vec::new(), Vec::new());
    let mut z0 = 0;
    let slab = slab.max(1);
    while z0 + 1 < nz {
        check_cancel(cancel)?;
        let z1 = (z0 + slab).min(nz - 1);
        let nzs = z1 - z0 + 1;
        let mut sub = Vec::with_capacity(nx * ny * nzs);
        for i in 0..nx {
            for j in 0..ny {
                for k in z0..=z1 {
                    sub.push(data[(i * ny + j) * nz + k].into());
                }
            }
        }
        let field = Field3::new([nx, ny, nzs], &sub)?;
        let (lo, hi) = field.min_max();
        if lo < 0.0 && 0.0 < hi {
            let m = marching_cubes(&field, 0.0, GradientDirection::Descent, true)?;
            if !m.faces.is_empty() {
                let base = vs.len();
                let zoff = z0 as f32;
                vs.extend(m.vertices.iter().map(|v| [v[0], v[1], v[2] + zoff]));
                fs.extend(
                    m.faces
                        .iter()
                        .map(|t| [t[0] as usize + base, t[1] as usize + base, t[2] as usize + base]),
                );
            }
        }
        z0 += slab;
    }
    Ok((vs, fs))
}

struct Grid3F32 {
    shape: [usize; 3],
    data: Vec<f32>,
}

fn gradient_f32(g: &Grid3F32, h: f32, axis: usize) -> Vec<f32> {
    let shape = g.shape;
    let strides = [shape[1] * shape[2], shape[2], 1];
    let s = strides[axis];
    let n = shape[axis];
    let two_h = 2.0f32 * h;
    let mut out = vec![0f32; g.data.len()];
    if n < 2 {
        return out;
    }
    for (idx, o) in out.iter_mut().enumerate() {
        let i = (idx / s) % n;
        *o = if i == 0 {
            (g.data[idx + s] - g.data[idx]) / h
        } else if i == n - 1 {
            (g.data[idx] - g.data[idx - s]) / h
        } else {
            (g.data[idx + s] - g.data[idx - s]) / two_h
        };
    }
    out
}

fn volume_report(vol: f64, lset: f64, dens: f64, dom: f64) -> Value {
    json!({
        "mesh_mm3": vol * 1e9, "levelset_mm3": lset * 1e9, "density_integral_mm3": dens * 1e9,
        "domain_mm3": dom * 1e9,
        "mesh_vs_levelset_rel": (vol - lset).abs() / lset.max(1e-30),
        "mesh_vs_density_rel": (vol - dens).abs() / dens.max(1e-30),
        "levelset_vs_density_rel": (lset - dens).abs() / dens.max(1e-30),
        "volume_fraction_mesh": vol / dom.max(1e-30),
        "volume_fraction_density": dens / dom.max(1e-30),
        "note": "levelset is the set the body is defined as and is the extraction's own round trip; density_integral \
                 is the physics volume fraction and equals it only for a sharp projection",
    })
}

fn centroid(v: &[Vec3], t: Tri) -> Vec3 {
    std::array::from_fn(|a| (v[t[0]][a] + v[t[1]][a] + v[t[2]][a]) / 3.0)
}

fn masked_sum(values: &[f64], mask: &[bool], want: bool) -> f64 {
    let sel: Vec<f64> = values.iter().zip(mask).filter(|(_, m)| **m == want).map(|(v, _)| *v).collect();
    pairwise_sum(&sel)
}

fn cap_facet_error(
    cap: &Cap<'_>,
    cen: &[Vec3],
    ar: &[f64],
    n_sample: usize,
    seed: u128,
) -> Result<Value, MeshError> {
    match cap {
        Cap::Box(b) => {
            let d: Vec<f64> = cen
                .iter()
                .map(|c| {
                    let q: Vec3 = std::array::from_fn(|a| (b.lo[a] - c[a]).max(c[a] - b.hi[a]));
                    let (x, y, z) = (q[0].max(0.0), q[1].max(0.0), q[2].max(0.0));
                    ((x * x + y * y + z * z).sqrt() + q[0].max(q[1]).max(q[2]).min(0.0)).abs()
                })
                .collect();
            let da: Vec<f64> = d.iter().zip(ar).map(|(a, b)| a * b).collect();
            Ok(json!({"kind": "box", "sampled": cen.len(), "exact": true,
                      "area_weighted_mm": pairwise_sum(&da) / pairwise_sum(ar) * 1e3,
                      "max_mm": d.iter().copied().fold(f64::NEG_INFINITY, f64::max) * 1e3}))
        }
        Cap::Mesh(m) => {
            let mut rng = implexity_core::rng::default_rng(seed);
            let k = n_sample.min(cen.len());
            let pick = rng
                .choice_without_replacement(cen.len(), k)
                .map_err(|e| MeshError::invalid(e.to_string()))?;
            let pick: Vec<usize> = pick.iter().map(|&p| crate::cast::idx(p)).collect();
            let pts: Vec<Vec3> = pick.iter().map(|&p| cen[p]).collect();
            let d = m.kernel.sdf_at(&m.v, &m.f, &pts)?;
            let ad: Vec<f64> = d.iter().map(|x| x.abs()).collect();
            let aa: Vec<f64> = pick.iter().map(|&p| ar[p]).collect();
            let da: Vec<f64> = ad.iter().zip(&aa).map(|(a, b)| a * b).collect();
            Ok(json!({"kind": "mesh", "sampled": k, "exact": false,
                      "area_weighted_mm": pairwise_sum(&da) / pairwise_sum(&aa) * 1e3,
                      "p99_mm": percentile(&ad, 99.0) * 1e3,
                      "max_mm": ad.iter().copied().fold(f64::NEG_INFINITY, f64::max) * 1e3}))
        }
    }
}

fn node_of(c: Vec3, origin: Vec3, h: f64, shape: [usize; 3]) -> usize {
    let n: [usize; 3] = std::array::from_fn(|a| {
        let v = crate::numeric::py_round((c[a] - origin[a]) / h);
        crate::numeric::clip(v, 0.0, (shape[a] - 1) as f64) as usize
    });
    (n[0] * shape[1] + n[1]) * shape[2] + n[2]
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn chord_report(
    ev: &dyn BodyEvaluator,
    v: &[Vec3],
    f: &[Tri],
    phi: &Grid3F32,
    origin: Vec3,
    h: f64,
    crease: &[bool],
    cap_owns: &[bool],
    cap: &Cap<'_>,
) -> Result<Value, MeshError> {
    let cen: Vec<Vec3> = f.iter().map(|t| centroid(v, *t)).collect();
    let ar = areas(v, f);
    let idx: Vec<Vec3> = cen.iter().map(|c| std::array::from_fn(|a| (c[a] - origin[a]) / h)).collect();
    let nid: Vec<usize> = cen.iter().map(|c| node_of(*c, origin, h, phi.shape)).collect();
    let is_cap: Vec<bool> = nid.iter().map(|&n| cap_owns[n]).collect();
    let on_crease: Vec<bool> = nid.iter().map(|&n| crease[n]).collect();
    let tot = pairwise_sum(&ar).max(1e-300);
    let mut out = Map::new();
    out.insert(
        "method".into(),
        json!(
            "|Phi| / |grad Phi| at triangle centroids, Phi evaluated pointwise (geff interpolated from the grid) and \
             |grad Phi| read from the sampled array.  Facets the CAP owns are excluded and measured separately against \
             the domain surface, because a sagitta against the lattice level set is not a statement about them"
        ),
    );
    out.insert("cap_area_fraction".into(), json!(masked_sum(&ar, &is_cap, true) / tot));
    out.insert("crease_area_fraction".into(), json!(masked_sum(&ar, &on_crease, true) / tot));
    out.insert(
        "crease_note".into(),
        json!(
            "the cap crease -- where the lattice surface meets the domain boundary -- is where max() of two distances \
             is not itself a distance; marching cubes rounds it over about one cell and this is the area affected"
        ),
    );
    if is_cap.iter().any(|&c| c) {
        let cc: Vec<Vec3> = cen.iter().zip(&is_cap).filter(|(_, m)| **m).map(|(c, _)| *c).collect();
        let ca: Vec<f64> = ar.iter().zip(&is_cap).filter(|(_, m)| **m).map(|(a, _)| *a).collect();
        out.insert("cap_facets".into(), cap_facet_error(cap, &cc, &ca, 20_000, 1)?);
    }
    let lev: Vec<usize> = (0..f.len()).filter(|&i| !is_cap[i]).collect();
    if lev.is_empty() {
        let cap_err = out.get("cap_facets").and_then(|c| c.get("area_weighted_mm")).and_then(Value::as_f64);
        let usable = cap_err.is_some_and(|e| e > 1e-9);
        let max_mm = out.get("cap_facets").and_then(|c| c.get("max_mm")).cloned().unwrap_or(Value::Null);
        out.insert("area_weighted_mm".into(), if usable { json!(cap_err) } else { Value::Null });
        out.insert("rms_mm".into(), Value::Null);
        out.insert("p99_mm".into(), Value::Null);
        out.insert("max_mm".into(), max_mm);
        out.insert("measured_on_area_fraction".into(), json!(0.0));
        out.insert("lattice_area_fraction".into(), json!(0.0));
        out.insert("mask_area_fraction".into(), json!(0.0));
        out.insert("chord_source".into(), json!(if usable { "cap" } else { "none" }));
        out.insert(
            "not_measurable".into(),
            json!(format!(
                "the lattice level set contributes no facet at this spacing (the density has not binarised, so the \
                 rho = 0.5 iso-surface inside the part is empty). {}",
                if usable {
                    "The chord quoted is the CAP facet error against the domain surface -- a real measurement of the \
                     surface that is written, but not a lattice sagitta."
                } else {
                    "The cap is analytic, so its facet error is zero and no chord can be quoted at all."
                }
            )),
        );
        return Ok(Value::Object(out));
    }
    let lcen: Vec<Vec3> = lev.iter().map(|&i| cen[i]).collect();
    let got = sample_exact(ev, &lcen, h)?;
    let h32 = crate::cast::f32_of(h);
    let mut g2 = vec![0.0; lev.len()];
    for c in 0..3 {
        let gc = gradient_f32(phi, h32, c);
        let gc64 = Grid3 { shape: phi.shape, data: gc.iter().map(|&x| f64::from(x)).collect() };
        for (m, &i) in lev.iter().enumerate() {
            let t = trilerp(&gc64, idx[i][0], idx[i][1], idx[i][2]);
            g2[m] += t * t;
        }
    }
    let sag: Vec<f64> = got.phi_lat.iter().zip(&g2).map(|(p, g)| p.abs() / g.sqrt().max(1e-12)).collect();
    let al: Vec<f64> = lev.iter().map(|&i| ar[i]).collect();
    let tl = pairwise_sum(&al).max(1e-300);
    let lat: Vec<bool> = got.mtilde.iter().map(|&m| m >= 0.99).collect();
    let sa: Vec<f64> = sag.iter().zip(&al).map(|(s, a)| s * a).collect();
    let s2: Vec<f64> = sag.iter().map(|s| s * s).collect();
    out.insert("area_weighted_mm".into(), json!(pairwise_sum(&sa) / tl * 1e3));
    out.insert("rms_mm".into(), json!((pairwise_sum(&s2) / s2.len() as f64).sqrt() * 1e3));
    out.insert("p99_mm".into(), json!(percentile(&sag, 99.0) * 1e3));
    out.insert("max_mm".into(), json!(sag.iter().copied().fold(f64::NEG_INFINITY, f64::max) * 1e3));
    out.insert("measured_on_area_fraction".into(), json!(tl / tot));
    out.insert("lattice_area_fraction".into(), json!(masked_sum(&al, &lat, true) / tot));
    out.insert("mask_area_fraction".into(), json!(masked_sum(&al, &lat, false) / tot));
    out.insert("chord_source".into(), json!("level_set"));
    if lat.iter().any(|&l| l) {
        let sl: Vec<f64> = got.g.iter().zip(&lat).filter(|(_, l)| **l).map(|(g, _)| g.abs()).collect();
        let aa: Vec<f64> = al.iter().zip(&lat).filter(|(_, l)| **l).map(|(a, _)| *a).collect();
        let sla: Vec<f64> = sl.iter().zip(&aa).map(|(s, a)| s * a).collect();
        out.insert(
            "lattice_q_minus_tau_area_weighted_mm".into(),
            json!(pairwise_sum(&sla) / pairwise_sum(&aa) * 1e3),
        );
        out.insert("lattice_q_minus_tau_p99_mm".into(), json!(percentile(&sl, 99.0) * 1e3));
    }
    if lat.iter().any(|&l| !l) {
        let sm: Vec<f64> = sa.iter().zip(&lat).filter(|(_, l)| !**l).map(|(s, _)| *s).collect();
        let am: Vec<f64> = al.iter().zip(&lat).filter(|(_, l)| !**l).map(|(a, _)| *a).collect();
        out.insert("mask_part_area_weighted_mm".into(), json!(pairwise_sum(&sm) / pairwise_sum(&am) * 1e3));
    }
    Ok(Value::Object(out))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Components {
    All,
    ConnectedTo(String),
}

impl Components {

    pub fn parse(s: &str) -> Result<Self, MeshError> {
        if s == "all" {
            Ok(Self::All)
        } else if let Some(g) = s.strip_prefix("connected_to:") {
            Ok(Self::ConnectedTo(g.to_string()))
        } else {
            Err(MeshError::invalid("components must be 'all' or 'connected_to:<group>'"))
        }
    }
    fn text(&self) -> String {
        match self {
            Self::All => "all".into(),
            Self::ConnectedTo(g) => format!("connected_to:{g}"),
        }
    }
}

fn touches_group(
    v: &[Vec3],
    f: &[Tri],
    cap: &Cap<'_>,
    want: &str,
    face_group: &Value,
) -> Result<Vec<bool>, MeshError> {
    let cen: Vec<Vec3> = f.iter().map(|t| centroid(v, *t)).collect();
    match cap {
        Cap::Box(b) => {
            let names = [
                ("x_lo", 0, b.lo[0]),
                ("x_hi", 0, b.hi[0]),
                ("y_lo", 1, b.lo[1]),
                ("y_hi", 1, b.hi[1]),
                ("z_lo", 2, b.lo[2]),
                ("z_hi", 2, b.hi[2]),
            ];
            let Some(&(_, a, val)) = names.iter().find(|(n, _, _)| *n == want) else {
                return Err(MeshError::invalid(format!(
                    "box face group {}; expected one of x_hi, x_lo, y_hi, y_lo, z_hi, z_lo",
                    implexity_core::py_repr::repr_str(want)
                )));
            };
            let tol = face_group.get("tol_m").and_then(Value::as_f64).unwrap_or(1e-9).max(1e-9);
            Ok(cen.iter().map(|c| (c[a] - val).abs() <= tol).collect())
        }
        Cap::Mesh(m) => {
            let gid: Vec<i64> = face_group
                .get("group_of_triangle")
                .and_then(Value::as_array)
                .ok_or_else(|| MeshError::invalid("face_group requires group_of_triangle"))?
                .iter()
                .map(|x| x.as_i64().unwrap_or(-1))
                .collect();
            let want_id: i64 = want.trim().parse().map_err(|_| {
                MeshError::invalid(format!(
                    "invalid literal for int() with base 10: {}",
                    implexity_core::py_repr::repr_str(want)
                ))
            })?;
            let (d, tri) = m.kernel.unsigned_distance(&m.v, &m.f, &cen)?;
            let band = face_group.get("band_m").and_then(Value::as_f64).unwrap_or(2e-4);
            Ok(d.iter()
                .zip(&tri)
                .map(|(dd, t)| *dd <= band && gid.get(*t).copied() == Some(want_id))
                .collect())
        }
    }
}

fn filter_components(
    v: &[Vec3],
    f: &[Tri],
    cap: &Cap<'_>,
    components: &Components,
    face_group: &Value,
    kernel: Option<&dyn SdfKernel>,
) -> Result<(Vec<Vec3>, Vec<Tri>, Value), MeshError> {
    let Components::ConnectedTo(want) = components else {
        return Err(MeshError::invalid("components must be 'all' or 'connected_to:<group>'"));
    };
    let (rows, cid, _cv, _vol_c) = topology::component_table(v, f, usize::MAX);
    let touch = touches_group(v, f, cap, want, face_group)?;
    let mut keep_ids: Vec<usize> = cid.iter().zip(&touch).filter(|(_, t)| **t).map(|(c, _)| *c).collect();
    keep_ids.sort_unstable();
    keep_ids.dedup();
    let voids: Vec<usize> = rows
        .iter()
        .filter(|r| r.volume_mm3 < 0.0 && !keep_ids.contains(&r.component))
        .map(|r| r.component)
        .collect();
    let mut readopted = Vec::new();
    if !voids.is_empty() && !keep_ids.is_empty() {
        let kernel =
            kernel.ok_or_else(|| MeshError::invalid("re-adopting voids needs the domain SDF kernel"))?;
        let fk: Vec<Tri> =
            f.iter().zip(&cid).filter(|(_, c)| keep_ids.contains(c)).map(|(t, _)| *t).collect();
        let cents: Vec<Vec3> = voids
            .iter()
            .map(|&c| {
                let pts: Vec<Vec3> = f
                    .iter()
                    .zip(&cid)
                    .filter(|(_, cc)| **cc == c)
                    .flat_map(|(t, _)| [v[t[0]], v[t[1]], v[t[2]]])
                    .collect();
                std::array::from_fn(|a| {
                    let col: Vec<f64> = pts.iter().map(|p| p[a]).collect();
                    pairwise_sum(&col) / col.len() as f64
                })
            })
            .collect();
        let w = kernel.winding_number(v, &fk, &cents)?;
        for (c, wi) in voids.iter().zip(&w) {
            if *wi > 0.5 {
                keep_ids.push(*c);
                readopted.push(*c);
            }
        }
        keep_ids.sort_unstable();
        keep_ids.dedup();
    }
    let sel: Vec<bool> = cid.iter().map(|c| keep_ids.contains(c)).collect();
    let mut used: Vec<usize> =
        f.iter().zip(&sel).filter(|(_, s)| **s).flat_map(|(t, _)| t.iter().copied()).collect();
    used.sort_unstable();
    used.dedup();
    let mut remap = vec![usize::MAX; v.len()];
    for (k, &u) in used.iter().enumerate() {
        remap[u] = k;
    }
    let dropped: Vec<&topology::ComponentRow> =
        rows.iter().filter(|r| !keep_ids.contains(&r.component)).collect();
    let mut largest: Vec<&topology::ComponentRow> = dropped.clone();
    largest.sort_by(|a, b| b.volume_mm3.abs().total_cmp(&a.volume_mm3.abs()));
    largest.truncate(8);
    let info = json!({
        "request": components.text(), "face_group": want, "components_before": rows.len(),
        "kept": keep_ids.len(), "voids_readopted": readopted, "dropped": dropped.len(),
        "dropped_volume_mm3": crate::numeric::py_sum(dropped.iter().map(|r| r.volume_mm3)),
        "dropped_largest": largest,
    });
    let v2 = used.iter().map(|&u| v[u]).collect();
    let f2 = f
        .iter()
        .zip(&sel)
        .filter(|(_, s)| **s)
        .map(|(t, _)| [remap[t[0]], remap[t[1]], remap[t[2]]])
        .collect();
    Ok((v2, f2, info))
}

pub struct ExtractOptions<'a> {
    pub components: Components,
    pub face_group: Value,
    pub cancel: Cancel<'a>,
    pub progress: Progress<'a>,
    pub measure: bool,
    pub snap: bool,
    pub want_cap_mask: bool,
    pub kernel: Option<&'a dyn SdfKernel>,
    pub slab: Option<usize>,
    pub max_samples: Option<usize>,
}

impl Default for ExtractOptions<'_> {
    fn default() -> Self {
        Self {
            components: Components::All,
            face_group: Value::Null,
            cancel: None,
            progress: None,
            measure: true,
            snap: true,
            want_cap_mask: false,
            kernel: None,
            slab: None,
            max_samples: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Extracted {
    pub vertices: Vec<Vec3>,
    pub faces: Vec<Tri>,
    pub stats: Map<String, Value>,
    pub cap_face_mask: Option<Vec<bool>>,
}



#[allow(clippy::too_many_lines)]
pub fn extract(
    ev: &dyn BodyEvaluator,
    cap: &Cap<'_>,
    h_mm: f64,
    opts: &ExtractOptions<'_>,
) -> Result<Extracted, MeshError> {
    let h_req = h_mm;
    let h_mm = if opts.snap { snap_spacing(ev, h_mm) } else { h_mm };
    let h = h_mm * MM;
    let (origin, shape) = grid_for(cap, h, PAD_CELLS);
    let n: usize = shape.iter().product();
    let budget = opts.max_samples.unwrap_or_else(max_samples);
    if n > budget {
        return Err(MeshError::Case(vec![
            format!(
                "spacing {} mm needs a {}x{}x{} = {}M-sample grid, over this service's budget of {}M",
                fmt_f(h_mm, 5),
                shape[0],
                shape[1],
                shape[2],
                fmt_f(n as f64 / 1e6, 1),
                fmt_g(budget as f64 / 1e6, 6)
            ),
            "ask for a coarser tolerance_mm, export a component rather than the whole domain, or raise the budget with \
             IMPLEXITY_MAX_BODY_SAMPLES"
                .into(),
        ]));
    }
    let t_all = Instant::now();
    let mut st = Map::new();
    st.insert("spacing_mm".into(), json!(h_mm));
    st.insert("spacing_requested_mm".into(), json!(h_req));
    st.insert(
        "spacing_snapped_to".into(),
        json!(format!("h_design / {}", crate::numeric::py_round(ev.h_design() * 1e3 / h_mm) as i64)),
    );
    st.insert("grid".into(), json!(shape));
    st.insert("samples".into(), json!(n));
    st.insert("origin_mm".into(), json!(origin.map(|v| v * 1e3)));

    let t0 = Instant::now();
    let phid: Vec<f32> = match cap {
        Cap::Box(b) => b.phi_grid(origin, h, shape),
        Cap::Mesh(m) => m.phi_grid(origin, h, shape, opts.cancel)?,
    };
    let occ: Vec<f64> = match cap {
        Cap::Box(b) => b.occupancy(origin, h, shape),
        Cap::Mesh(_) => MeshCap::occupancy(&phid, h),
    }
    .iter()
    .map(|&x| f64::from(x))
    .collect();
    st.insert("cap_seconds".into(), json!(py_round_digits(t0.elapsed().as_secs_f64(), 2)));
    st.insert("cap".into(), cap.describe());
    if let Cap::Mesh(m) = cap {
        st.insert("cap_verification".into(), m.verify(origin, h, shape, &phid, 12_000, 0)?);
    }

    let t0 = Instant::now();
    let fprog = opts.progress.map(|p| move |f: f64| p(0.05 + 0.45 * f));
    let slab = opts.slab.unwrap_or_else(slab_cells);
    let fields = eval_fields(
        ev,
        origin,
        h,
        shape,
        &occ,
        h.powi(3),
        slab,
        opts.cancel,
        fprog.as_ref().map(|f| f as &dyn Fn(f64)),
    )?;
    drop(occ);
    st.insert("field_seconds".into(), json!(py_round_digits(t0.elapsed().as_secs_f64(), 2)));
    st.insert("geff_spacing_mm".into(), json!(ev.h_design() * 1e3));
    st.insert("geff_blur_radius".into(), json!(ev.blur_radius_design()));
    st.insert(
        "geff_note".into(),
        json!(
            "geff is evaluated ONCE on the design's own analysis grid, at the design's own spacing and phase, and \
             interpolated -- not re-blurred at the export spacing.  Its blur radius is an integer, so re-blurring gives \
             a different solid at every tolerance (5.32 % of volume, measured)"
        ),
    );
    let mut phi = fields.phi;
    let h32 = crate::cast::f32_of(h);
    let crease: Vec<bool> = phid.iter().zip(&phi).map(|(a, b)| a.abs() < h32 && b.abs() < h32).collect();
    st.insert("crease_nodes".into(), json!(crease.iter().filter(|&&c| c).count()));
    let cap_owns: Vec<bool> = phid.iter().zip(&phi).map(|(a, b)| a >= b).collect();
    for (p, d) in phi.iter_mut().zip(&phid) {
        *p = p.max(*d);
    }
    drop(phid);
    st.insert("deadband_nodes".into(), json!(deplateau(&mut phi, crate::cast::f32_of(DEADBAND_FRAC * h))));
    st.insert("deadband_mm".into(), json!(DEADBAND_FRAC * h * 1e3));
    let shell = crate::cast::f32_of(BAND_CELLS * h);
    let [nx, ny, nz] = shape;
    for i in 0..nx {
        for j in 0..ny {
            for k in 0..nz {
                if i == 0 || i == nx - 1 || j == 0 || j == ny - 1 || k == 0 || k == nz - 1 {
                    phi[(i * ny + j) * nz + k] = shell;
                }
            }
        }
    }
    let phi = Grid3F32 { shape, data: phi };

    let t0 = Instant::now();
    let (vi, fi) = mc_slabs(&phi, slab, opts.cancel)?;
    if let Some(p) = opts.progress {
        p(0.75);
    }
    let vi64: Vec<Vec3> = vi.iter().map(|p| p.map(f64::from)).collect();
    let (vw, mut faces, ndegen) = weld_exact(&vi64, &fi);
    drop(vi64);

    let mut verts: Vec<Vec3> = vw
        .iter()
        .map(|p| std::array::from_fn(|a| f64::from(crate::cast::f32_of(p[a]) * h32) + origin[a]))
        .collect();
    st.insert("mc_seconds".into(), json!(py_round_digits(t0.elapsed().as_secs_f64(), 2)));
    st.insert("degenerate_triangles_dropped".into(), json!(ndegen));

    let mut vol = signed_volume(&verts, &faces);
    st.insert("winding_flipped".into(), json!(vol < 0.0));
    if vol < 0.0 {
        for t in &mut faces {
            *t = [t[2], t[1], t[0]];
        }
        vol = -vol;
    }
    if opts.components != Components::All && !faces.is_empty() {
        let kernel = opts.kernel.or(match cap {
            Cap::Mesh(m) => Some(m.kernel),
            Cap::Box(_) => None,
        });
        let (v2, f2, info) =
            filter_components(&verts, &faces, cap, &opts.components, &opts.face_group, kernel)?;
        verts = v2;
        faces = f2;
        st.insert("component_filter".into(), info);
        vol = signed_volume(&verts, &faces);
    }
    let tp = topology_fast(&verts, &faces);
    if let Value::Object(m) = tp.to_json() {
        for (k, v) in m {
            st.insert(format!("topology_{k}"), v);
        }
    }
    let orient = orientation_report(&faces);
    let consistent = orient.consistent;
    st.insert("orientation".into(), orient.to_json());
    st.insert("triangles".into(), json!(faces.len()));
    st.insert("vertices".into(), json!(verts.len()));
    st.insert("area_mm2".into(), json!(pairwise_sum(&areas(&verts, &faces)) * 1e6));
    st.insert("signed_volume_mm3".into(), json!(vol * 1e9));
    st.insert(
        "watertight".into(),
        json!(tp.boundary_edges == 0 && tp.nonmanifold_edges == 0 && consistent && vol > 0.0),
    );
    st.insert("volume".into(), volume_report(vol, fields.lset, fields.dens, cap.volume_m3()));
    if opts.measure && !faces.is_empty() {
        st.insert(
            "chord".into(),
            chord_report(ev, &verts, &faces, &phi, origin, h, &crease, &cap_owns, cap)?,
        );
    }
    let cap_face_mask = (opts.want_cap_mask && !faces.is_empty())
        .then(|| faces.iter().map(|t| cap_owns[node_of(centroid(&verts, *t), origin, h, shape)]).collect());
    let (rows, _cid, _cv, vol_c) = topology::component_table(&verts, &faces, 64);
    st.insert("components_table".into(), serde_json::to_value(&rows).unwrap_or(Value::Null));
    st.insert("component_count".into(), json!(tp.components));
    st.insert("components_negative_volume".into(), json!(vol_c.iter().filter(|&&v| v < 0.0).count()));
    st.insert(
        "components_note".into(),
        json!(
            "a component with negative signed volume is an INTERNAL VOID (a closed pore inside the solid), not a \
             defect; a second positive component is a disconnected piece of the design and is a real finding -- use \
             components=connected_to:<face_group> to keep only what reaches a named face"
        ),
    );
    st.insert("seconds".into(), json!(py_round_digits(t_all.elapsed().as_secs_f64(), 2)));
    Ok(Extracted { vertices: verts, faces, stats: st, cap_face_mask })
}

#[must_use]
pub fn fit_law(h1: f64, e1: f64, n1: f64, h2: f64, e2: f64, n2: f64) -> (f64, f64) {
    let lr = (h1 / h2).ln();
    let (p, q) = if lr == 0.0 {
        (2.0, -2.0)
    } else {
        ((e1.max(1e-30) / e2.max(1e-30)).ln() / lr, (n1.max(1.0) / n2.max(1.0)).ln() / lr)
    };
    (crate::numeric::clip(p, EXP_CLAMP.0, EXP_CLAMP.1), crate::numeric::clip(q, -3.0, -1.0))
}

#[must_use]
pub fn choose_spacing(eps_mm: f64, h0_mm: f64, eps0_mm: f64, p: f64) -> f64 {
    if eps0_mm <= 0.0 {
        return h0_mm;
    }
    h0_mm * (eps_mm.max(1e-9) / eps0_mm).powf(1.0 / p)
}

#[must_use]
pub fn extrapolate(
    h0_mm: f64,
    eps0_mm: f64,
    tris0: f64,
    targets: &[f64],
    p: f64,
    q: f64,
) -> Map<String, Value> {
    let mut out = Map::new();
    for &e in targets {
        let h = choose_spacing(e, h0_mm, eps0_mm, p);
        out.insert(
            fmt_f(e, 4),
            json!({"tolerance_mm": e, "spacing_mm": py_round_digits(h, 6),
                   "triangles": (tris0 * (h / h0_mm).powf(q)).ceil() as i64}),
        );
    }
    out
}

#[must_use]
pub fn case_hash(case_doc: &Value) -> String {
    let text =
        implexity_core::json::dumps(case_doc, &implexity_core::json::DumpOptions::default().sorted(true));
    implexity_io::digest::blake2b_hex(text.as_bytes(), 8)
}

#[must_use]
pub fn provenance(
    snap: &DesignSnapshot,
    case_doc: &Value,
    stats: &Map<String, Value>,
    tolerance_mm: f64,
    fmt: &str,
    extra: Option<&Map<String, Value>>,
) -> Value {
    let meta = &snap.meta;
    let vol = stats.get("volume").cloned().unwrap_or_else(|| json!({}));
    let ch = stats.get("chord").cloned().unwrap_or_else(|| json!({}));
    let g = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    let s = |k: &str| stats.get(k).cloned().unwrap_or(Value::Null);
    let list =
        |k: &str| case_doc.get(k).and_then(Value::as_array).cloned().map_or_else(|| json!([]), Value::Array);
    let m = |k: &str| meta.get(k).cloned().unwrap_or(Value::Null);
    let t_offset_mm = meta.get("t_offset").and_then(Value::as_f64).map_or(Value::Null, |t| json!(t * 1e3));
    let source = match meta.get("source") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => crate::exporters::py_str(other),
        None => "None".into(),
    };
    let mut out = json!({
        "schema": "implexity-body-provenance/1",
        "produced_by": "implexity bodyexport",
        "design_version": snap.version,
        "design_source": source,
        "case_name": g(case_doc, "name"),
        "case_hash": case_hash(case_doc),
        "case_grid": list("grid"),
        "case_h_mm": g(case_doc, "h_mm"),
        "domain_mm": list("domain_mm"),
        "domain_mesh": case_doc.get("domain").filter(|d| !d.is_null()).and_then(|d| d.get("mesh")).cloned().unwrap_or(Value::Null),
        "objective": case_doc.get("objective").cloned().unwrap_or_else(|| json!("default")),
        "volfrac_target": g(case_doc, "volfrac"),
        "volume_fraction_density": g(&vol, "volume_fraction_density"),
        "volume_fraction_mesh": g(&vol, "volume_fraction_mesh"),
        "continuation": {"interface_w": m("interface_w"), "beta_mask": m("beta_mask"), "beta_mat": m("beta_mat"),
                         "beta_topo": m("beta_topo"), "t_offset_mm": t_offset_mm,
                         "provenance": meta.get("continuation_provenance").cloned().unwrap_or_else(|| json!({}))},
        "lattice": {"period_mm": m("period_mm"),
                    "control_shape": meta.get("control_shape").and_then(Value::as_array).cloned().map_or_else(|| json!([]), Value::Array)},
        "format": fmt,
        "tolerance_requested_mm": tolerance_mm,
        "tolerance_achieved_mm": g(&ch, "area_weighted_mm"),
        "tolerance_definition": g(&ch, "method"),
        "extraction": {"spacing_mm": s("spacing_mm"), "grid": s("grid"), "triangles": s("triangles"),
                       "vertices": s("vertices"), "watertight": s("watertight"),
                       "boundary_edges": s("topology_boundary_edges"),
                       "nonmanifold_edges": s("topology_nonmanifold_edges"),
                       "components": s("component_count"), "genus": s("topology_genus"),
                       "signed_volume_mm3": s("signed_volume_mm3")},
        "units": "millimetre",
        "regenerable": "the recipe JSON beside this file (when the physics package writes one) reconstructs the \
                        density field; re-running the body export at the same tolerance reproduces this mesh",
    });
    if let (Some(extra), Value::Object(o)) = (extra, &mut out) {
        for (k, v) in extra {
            o.insert(k.clone(), v.clone());
        }
    }
    out
}

#[must_use]
pub fn calibration_divisors(ev: &dyn BodyEvaluator, cap: &Cap<'_>) -> (f64, f64) {
    let mut div = calibrate_div();
    loop {
        let fine = snap_spacing(ev, ev.h_design() * 1e3 / (2.0 * div)) * MM;
        let (_o, shape) = grid_for(cap, fine, PAD_CELLS);
        if shape.iter().product::<usize>() <= max_samples() || div <= 1.0 {
            return (div, 2.0 * div);
        }
        div = (div / 2.0).max(1.0);
    }
}

pub type RecipeWriter<'a> = &'a dyn Fn(&DesignSnapshot, &Value, &Path) -> Result<Option<Value>, String>;

pub struct BuildBodyOptions<'a> {
    pub tolerance_mm: f64,
    pub formats: Vec<String>,
    pub components: Components,
    pub face_group: Value,
    pub name: String,
    pub cancel: Cancel<'a>,
    pub progress: Progress<'a>,
    pub calibrate: bool,
    pub spacing_mm: Option<f64>,
    pub targets: Vec<f64>,
    pub step_merge: bool,
    pub step_schema: String,
    pub recipe: Option<RecipeWriter<'a>>,
    pub kernel: Option<&'a dyn SdfKernel>,
    pub slab: Option<usize>,
}

impl Default for BuildBodyOptions<'_> {
    fn default() -> Self {
        Self {
            tolerance_mm: 0.02,
            formats: vec!["stl".into()],
            components: Components::All,
            face_group: Value::Null,
            name: "body".into(),
            cancel: None,
            progress: None,
            calibrate: true,
            spacing_mm: None,
            targets: vec![0.05, 0.02, 0.01, 0.005],
            step_merge: true,
            step_schema: "AP214".into(),
            recipe: None,
            kernel: None,
            slab: None,
        }
    }
}

fn file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map_or(0, |m| m.len())
}

pub type Body = (Map<String, Value>, Vec<Vec3>, Vec<Tri>);



#[allow(clippy::too_many_lines)]
pub fn build_body(
    ev: &dyn BodyEvaluator,
    case_doc: &Value,
    cap: &Cap<'_>,
    out_dir: &Path,
    opts: &BuildBodyOptions<'_>,
) -> Result<Body, MeshError> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| MeshError::io(format!("creating {}", out_dir.display()), e))?;
    let mut rep = Map::new();
    rep.insert("schema".into(), json!("implexity-body/1"));
    rep.insert(
        "requested".into(),
        json!({"tolerance_mm": opts.tolerance_mm, "formats": opts.formats,
               "components": opts.components.text(), "cap": cap.kind()}),
    );
    if let Some(p) = opts.progress {
        p(0.02);
    }
    let h: f64;
    if opts.spacing_mm.is_none() && opts.calibrate {
        let mut levels: Vec<Value> = Vec::new();
        let (d1, d2) = calibration_divisors(ev, cap);
        for div in [d1, d2] {
            let hc = ev.h_design() * 1e3 / div;
            let eo = ExtractOptions {
                cancel: opts.cancel,
                measure: true,
                slab: opts.slab,
                ..ExtractOptions::default()
            };
            let x = extract(ev, cap, hc, &eo)?;
            let sc = &x.stats;
            let chord = sc.get("chord").ok_or_else(|| {
                MeshError::invalid(
                    "a calibration extraction produced no triangles, so no chord could be measured",
                )
            })?;
            levels.push(json!({"spacing_mm": sc["spacing_mm"], "triangles": sc["triangles"],
                               "chord_area_weighted_mm": chord["area_weighted_mm"],
                               "watertight": sc["watertight"], "components": sc["component_count"],
                               "genus": sc["topology_genus"], "seconds": sc["seconds"]}));
        }
        if levels.iter().any(|l| l["chord_area_weighted_mm"].is_null()) {
            h = levels[1]["spacing_mm"].as_f64().unwrap_or(0.0);
            rep.insert(
                "calibration".into(),
                json!({"levels": levels, "skipped": true, "spacing_mm": h,
                       "law": format!("no chord could be measured at either calibration spacing: the lattice level set \
                                       contributes no facet and the cap is analytic, so tolerance_mm could not be \
                                       honoured and the body was built at h_design/{}", fmt_g(d2, 6))}),
            );
            rep.insert("estimate".into(), json!({}));
        } else {
            let lv = |i: usize, k: &str| levels[i][k].as_f64().unwrap_or(0.0);
            let (pexp, qexp) = fit_law(
                lv(0, "spacing_mm"),
                lv(0, "chord_area_weighted_mm"),
                lv(0, "triangles"),
                lv(1, "spacing_mm"),
                lv(1, "chord_area_weighted_mm"),
                lv(1, "triangles"),
            );
            let (h0, eps0) = (lv(1, "spacing_mm"), lv(1, "chord_area_weighted_mm"));
            let tris0 = lv(1, "triangles");
            rep.insert(
                "calibration".into(),
                json!({"levels": levels, "chord_exponent": pexp, "triangle_exponent": qexp,
                       "law": format!("chord ~ C h^{}, triangles ~ K h^{}, both fitted on this design at two spacings",
                                      fmt_f(pexp, 2), fmt_f(qexp, 2))}),
            );
            h = if eps0 == 0.0 { h0 } else { choose_spacing(opts.tolerance_mm, h0, eps0, pexp) };
            rep.insert(
                "estimate".into(),
                if eps0 == 0.0 {
                    json!({})
                } else {
                    Value::Object(extrapolate(h0, eps0, tris0, &opts.targets, pexp, qexp))
                },
            );
        }
    } else {
        h = opts.spacing_mm.unwrap_or_else(|| ev.h_design() * 1e3 / calibrate_div());
        rep.insert("calibration".into(), json!({"skipped": true, "spacing_mm": h}));
    }
    if let Some(p) = opts.progress {
        p(0.10);
    }
    let want_step = opts.formats.iter().any(|f| f == "step");
    let eo = ExtractOptions {
        components: opts.components.clone(),
        face_group: opts.face_group.clone(),
        cancel: opts.cancel,
        progress: opts.progress,
        measure: true,
        snap: true,
        want_cap_mask: want_step,
        kernel: opts.kernel,
        slab: opts.slab,
        max_samples: None,
    };
    let x = extract(ev, cap, h, &eo)?;
    let st = x.stats.clone();
    rep.insert("extraction".into(), Value::Object(st.clone()));
    let mut files = Map::new();
    let stem = out_dir.join(&opts.name);
    let with_suffix = |s: &str| PathBuf::from(format!("{}{s}", stem.display()));
    let rp = with_suffix("_recipe.json");
    let snap = ev.snapshot();
    match opts.recipe.map(|w| w(&snap, case_doc, &rp)) {
        None | Some(Ok(None)) => {
            files.insert(
                "recipe".into(),
                json!({"skipped": "no active physics package writes a design recipe"}),
            );
        }
        Some(Ok(Some(rec))) => {
            files.insert("recipe".into(), json!({"path": rp.display().to_string(), "bytes": file_size(&rp), "schema": rec.get("schema")}));
        }
        Some(Err(e)) => {
            files.insert("recipe".into(), json!({"error": e}));
        }
    }
    let mut prov = provenance(&snap, case_doc, &st, opts.tolerance_mm, &opts.formats.join("+"), None);
    prov["files"] = json!({});
    let pp = with_suffix("_provenance.json");
    let mut blobs: Vec<(String, Vec<u8>)> = Vec::new();
    if rp.is_file()
        && let Ok(b) = std::fs::read(&rp)
    {
        blobs.push(("/Metadata/recipe.json".into(), b));
    }
    let mut options = Map::new();
    options.insert("step_merge".into(), json!(opts.step_merge));
    options.insert("step_schema".into(), json!(opts.step_schema));
    for fmt in &opts.formats {
        let entry = crate::exporters::get(fmt)?;
        let p = with_suffix(&format!(".{}", entry.extension));
        let t0 = Instant::now();
        let prov_now = prov.clone();
        let mut w = crate::exporters::BodyWrite {
            path: p.clone(),
            vertices: &x.vertices,
            faces: &x.faces,
            name: &opts.name,
            stats: &Value::Object(st.clone()),
            provenance: &prov_now,
            blobs: &blobs,
            cap_face_mask: x.cap_face_mask.as_deref(),
            options: &options,
            report: &mut rep,
        };
        let nb = (entry.write)(&mut w)?;
        let rec = json!({"path": p.display().to_string(), "bytes": nb,
                         "write_seconds": py_round_digits(t0.elapsed().as_secs_f64(), 2)});
        prov["files"][fmt.as_str()] = rec.clone();
        files.insert(fmt.clone(), rec);
    }
    let text =
        implexity_core::json::dumps(&prov, &implexity_core::json::DumpOptions::indented(1).sorted(true));
    crate::formats::write_file(&pp, text.as_bytes())?;
    files.insert("provenance".into(), json!({"path": pp.display().to_string(), "bytes": file_size(&pp)}));
    if let Some(sa) = rep.get("step").map(|s| (s["accepted"].clone(), s["written"] == "merged")) {
        let (acc, merged) = sa;
        if let Some(Value::Object(fs)) = prov.get_mut("files").and_then(|f| f.get_mut("step")) {
            fs.insert("valid_solid".into(), acc["valid_solid"].clone());
            fs.insert("faces".into(), acc["faces"].clone());
            fs.insert("faces_unmerged".into(), acc["faces_unmerged"].clone());
            fs.insert("step_volume_mm3".into(), acc["volume_mm3"].clone());
            fs.insert("step_volume_vs_mesh_rel".into(), acc["volume_vs_mesh_rel"].clone());
            fs.insert("schema".into(), acc["schema"].clone());
            fs.insert("merged".into(), json!(merged));
        }
    }

    if let Some(step) = prov.get("files").and_then(|f| f.get("step")) {
        files.insert("step".into(), step.clone());
    }
    rep.insert("files".into(), Value::Object(files));
    rep.insert("provenance".into(), prov);
    let mut accepted = json!({
        "boundary_edges": st["topology_boundary_edges"], "nonmanifold_edges": st["topology_nonmanifold_edges"],
        "orientation_consistent": st["orientation"]["consistent"],
        "signed_volume_positive": st["signed_volume_mm3"].as_f64().unwrap_or(0.0) > 0.0,
        "volume_vs_levelset_rel": st["volume"]["mesh_vs_levelset_rel"],
        "volume_vs_density_rel": st["volume"]["mesh_vs_density_rel"],
        "chord_area_weighted_mm": st.get("chord").map_or(Value::Null, |c| c["area_weighted_mm"].clone()),
        "components": st["component_count"], "genus": st["topology_genus"], "watertight": st["watertight"],
    });
    if let Some(sa) = rep.get("step").map(|s| s["accepted"].clone()) {
        for (k, src) in [
            ("step_valid_solid", "valid_solid"),
            ("step_closed_shell", "closed_shell"),
            ("step_solids", "solids"),
            ("step_faces", "faces"),
            ("step_faces_unmerged", "faces_unmerged"),
            ("step_volume_mm3", "volume_mm3"),
            ("step_volume_vs_mesh_rel", "volume_vs_mesh_rel"),
            ("step_roundtrip_valid", "roundtrip_valid"),
            ("step_schema", "schema"),
        ] {
            accepted[k] = sa[src].clone();
        }
    }
    rep.insert("accepted".into(), accepted);
    if let Some(p) = opts.progress {
        p(1.0);
    }
    Ok((rep, x.vertices, x.faces))
}
