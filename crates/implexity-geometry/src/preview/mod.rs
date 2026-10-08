// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



pub mod real;
pub mod synthetic;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use crate::boxes::{MAX_PREVIEW_SAMPLES, SampleBox};
use crate::error::{GResult, GeometryError};

pub use real::{RealEvaluator, RealGeometry};
pub use synthetic::{SyntheticEvaluator, SyntheticGeometry, register_backends, synthetic_design};

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub shape: Vec<usize>,
    pub data: Vec<f64>,
}

impl Channel {
    #[must_use]
    pub fn spatial(&self) -> [usize; 3] {
        let n = self.shape.len();
        [self.shape[n - 3], self.shape[n - 2], self.shape[n - 1]]
    }

    #[must_use]
    pub fn component(&self, i: usize) -> &[f64] {
        let s = self.spatial();
        let n = s[0] * s[1] * s[2];
        if self.shape.len() == 3 { &self.data } else { &self.data[i * n..(i + 1) * n] }
    }
}

pub type Params = BTreeMap<String, Channel>;


pub fn channel<'a>(params: &'a Params, name: &str) -> GResult<&'a Channel> {
    params.get(name).ok_or_else(|| GeometryError::Key(name.to_string()))
}

pub const CHANNELS: [&str; 9] = ["a", "m", "dphi", "s", "w", "nu", "w2", "res", "c"];

struct DesignState {
    params: Params,
    meta: Map<String, Value>,
    version: u64,
}

pub struct Design {
    state: Mutex<DesignState>,
}

fn lock_err() -> GeometryError {
    GeometryError::Value("design state lock poisoned".into())
}

impl Design {
    #[must_use]
    pub fn new(params: Params, meta: Map<String, Value>) -> Self {
        Self { state: Mutex::new(DesignState { params, meta, version: 0 }) }
    }


    pub fn from_npz(
        path: &std::path::Path,
        domain_mm: [f64; 3],
        design_grid: Option<[usize; 3]>,
        period_mm: f64,
    ) -> GResult<Self> {
        let npz = implexity_io::npz::load_file(path).map_err(|e| GeometryError::Io(e.to_string()))?;
        let mut params = Params::new();
        for k in CHANNELS {
            if let Some(a) = npz.get(k) {
                let arr = a.to_f64().ok_or_else(|| GeometryError::Value(format!("{k} is not numeric")))?;
                params.insert(
                    k.to_string(),
                    Channel { shape: arr.shape().to_vec(), data: arr.iter().copied().collect() },
                );
            }
        }
        if params.is_empty() {
            let list = crate::pyfmt::PyObj::Tuple(
                CHANNELS.iter().map(|c| crate::pyfmt::PyObj::Str((*c).into())).collect(),
            )
            .repr();
            return Err(GeometryError::Value(format!("{} holds none of {list}", path.display())));
        }
        let mut prov = Map::new();
        let mut scal = |name: &str, default: f64| -> f64 {
            let here = npz
                .get(name)
                .and_then(implexity_io::npy::NpyArray::to_f64)
                .and_then(|a| a.iter().next().copied());
            prov.insert(name.into(), json!(if here.is_some() { "run" } else { "assumed" }));
            here.unwrap_or(default)
        };
        let iw = scal("interface_w", 0.5);
        let bm = scal("beta_mask", 128.0);
        let bmat = scal("beta_mat", 64.0);
        let bt = scal("beta_topo", 64.0);
        let to = scal("t_offset", 0.0);
        let cs = params.get("a").map_or([0; 3], Channel::spatial);
        let mut meta = Map::new();
        meta.insert("source".into(), json!(path.to_string_lossy()));
        meta.insert("domain_mm".into(), json!(domain_mm));
        meta.insert("control_shape".into(), json!(cs));
        meta.insert("design_grid".into(), json!(design_grid.unwrap_or([30, 60, 120])));
        meta.insert("interface_w".into(), json!(iw));
        meta.insert("beta_mask".into(), json!(bm));
        meta.insert("beta_mat".into(), json!(bmat));
        meta.insert("beta_topo".into(), json!(bt));
        meta.insert("t_offset".into(), json!(to));
        meta.insert("period_mm".into(), json!(period_mm));
        meta.insert("channels".into(), json!(params.keys().collect::<Vec<_>>()));
        meta.insert("continuation_provenance".into(), Value::Object(prov));
        Ok(Self::new(params, meta))
    }


    pub fn snapshot(&self) -> GResult<(Params, u64)> {
        let s = self.state.lock().map_err(|_| lock_err())?;
        Ok((s.params.clone(), s.version))
    }


    pub fn snapshot_full(&self) -> GResult<(Params, Map<String, Value>, u64)> {
        let s = self.state.lock().map_err(|_| lock_err())?;
        Ok((s.params.clone(), s.meta.clone(), s.version))
    }


    pub fn meta(&self) -> GResult<Map<String, Value>> {
        Ok(self.state.lock().map_err(|_| lock_err())?.meta.clone())
    }


    pub fn set_meta(&self, meta: Map<String, Value>) -> GResult<()> {
        self.state.lock().map_err(|_| lock_err())?.meta = meta;
        Ok(())
    }


    pub fn replace_design(
        &self,
        params: Params,
        meta_updates: Option<&Map<String, Value>>,
        source: Option<&str>,
    ) -> GResult<u64> {
        let mut s = self.state.lock().map_err(|_| lock_err())?;
        s.params = params;
        if let Some(u) = meta_updates {
            for (k, v) in u {
                s.meta.insert(k.clone(), v.clone());
            }
        }
        if let Some(src) = source {
            s.meta.insert("source".into(), json!(src));
        }
        let channels: Vec<String> = s.params.keys().cloned().collect();
        s.meta.insert("channels".into(), json!(channels));
        let cs = s.params.get("a").map_or([0; 3], Channel::spatial);
        s.meta.insert("control_shape".into(), json!(cs));
        s.version += 1;
        Ok(s.version)
    }


    pub fn apply_delta(
        &self,
        channel: &str,
        delta: Option<f64>,
        scale: Option<f64>,
        absolute: Option<f64>,
        index: Option<usize>,
    ) -> GResult<u64> {
        let mut s = self.state.lock().map_err(|_| lock_err())?;
        let Some(arr) = s.params.get_mut(channel) else {
            return Err(GeometryError::Value(crate::pyfmt::str_repr(channel)));
        };
        let range = match index {
            Some(i) if arr.shape.len() > 3 => {
                let sp = arr.spatial();
                let n = sp[0] * sp[1] * sp[2];
                i * n..(i + 1) * n
            }
            _ => 0..arr.data.len(),
        };
        for v in &mut arr.data[range] {
            if let Some(a) = absolute {
                *v = a;
            }
            if let Some(sc) = scale {
                *v *= sc;
            }
            if let Some(d) = delta {
                *v += d;
            }
        }
        s.version += 1;
        Ok(s.version)
    }
}

#[must_use]
#[inline]
pub fn base_index(x: f64, hi: f64) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let t = x as i64 as f64;
    let fl = if t > x { t - 1.0 } else { t };
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let v = fl.clamp(0.0, hi) as usize;
    v
}

#[must_use]
pub fn trilerp_np(f: &[f64], shape: [usize; 3], ci: [f64; 3]) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let b: [usize; 3] = std::array::from_fn(|a| base_index(ci[a], shape[a] as f64 - 2.0));
    #[allow(clippy::cast_precision_loss)]
    let fr: [f64; 3] = std::array::from_fn(|a| (ci[a] - b[a] as f64).clamp(0.0, 1.0));
    let at = |i: usize, j: usize, k: usize| f[(i * shape[1] + j) * shape[2] + k];
    let [x0, y0, z0] = b;
    let (x1, y1, z1) = (x0 + 1, y0 + 1, z0 + 1);
    let [fx, fy, fz] = fr;
    let c00 = at(x0, y0, z0) * (1.0 - fx) + at(x1, y0, z0) * fx;
    let c01 = at(x0, y0, z1) * (1.0 - fx) + at(x1, y0, z1) * fx;
    let c10 = at(x0, y1, z0) * (1.0 - fx) + at(x1, y1, z0) * fx;
    let c11 = at(x0, y1, z1) * (1.0 - fx) + at(x1, y1, z1) * fx;
    (c00 * (1.0 - fy) + c10 * fy) * (1.0 - fz) + (c01 * (1.0 - fy) + c11 * fy) * fz
}

#[must_use]
pub fn soft_floor_np(x: f64, floor: f64) -> f64 {
    let w = crate::lattice::synth::FLOOR_WIDTH_REL * floor;
    let z = (x - floor) / w;
    floor + w * (z.max(0.0) + (-z.abs()).exp().ln_1p())
}

#[must_use]
pub fn sigmoid_np(x: f64) -> f64 {
    0.5 * ((0.5 * x).tanh() + 1.0)
}

pub type BoxFields = BTreeMap<String, Vec<f64>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    All,
    Geff,
    Fast,
}

pub type Continuation = [f64; 5];

pub trait TwoRate: Send + Sync {
    fn name(&self) -> &'static str;

    fn rebind(&self) -> GResult<bool> {
        Ok(false)
    }
    fn design(&self) -> &Arc<Design>;
    fn h_design(&self) -> f64;
    fn interface_eps(&self) -> f64;
    fn halo(&self, h: f64) -> (usize, usize);
    fn blur_r(&self, h: f64) -> usize;
    fn prep(&self, b: &SampleBox, boundary: &str) -> Vec<[f64; 3]>;

    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        params: &Params,
        pts: &[[f64; 3]],
        shape: [usize; 3],
        h: f64,
        axes: [[f64; 3]; 3],
        r: usize,
        mode: RunMode,
        cont: &Continuation,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> GResult<BoxFields>;
    fn on_snapshot(&self, _meta: &Map<String, Value>, _params: &Params) {}

    fn cont_vec(&self, meta: &Map<String, Value>) -> Continuation {
        let g = |k: &str| meta.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        [g("t_offset"), g("beta_mask"), g("beta_mat"), g("beta_topo"), g("interface_w") * self.h_design()]
    }


    fn evaluate(
        &self,
        b: &SampleBox,
        fields: &[&str],
        cancel: Option<&dyn Fn() -> bool>,
        boundary: &str,
        geff_stride: usize,
    ) -> GResult<(BoxFields, u64)> {
        let (params, meta, version) = self.design().snapshot_full()?;
        self.on_snapshot(&meta, &params);
        let cont = self.cont_vec(&meta);
        let out = if geff_stride <= 1 {
            self.single_rate(&params, b, fields, cancel, boundary, &cont)?
        } else {
            self.two_rate(&params, b, fields, cancel, boundary, geff_stride, &cont)?
        };
        Ok((out, version))
    }


    fn single_rate(
        &self,
        params: &Params,
        b: &SampleBox,
        fields: &[&str],
        cancel: Option<&dyn Fn() -> bool>,
        boundary: &str,
        cont: &Continuation,
    ) -> GResult<BoxFields> {
        let (pad, r) = self.halo(b.h);
        let big = b.grown(pad);

        big.admit(MAX_PREVIEW_SAMPLES)?;
        let pts = self.prep(&big, boundary);
        if cancel.is_some_and(|c| c()) {
            return Err(GeometryError::Cancelled);
        }
        let res = self.run(params, &pts, big.shape, big.h, big.axes, r, RunMode::All, cont, cancel)?;
        Ok(res
            .into_iter()
            .filter(|(k, _)| fields.contains(&k.as_str()))
            .map(|(k, v)| (k, crop(&v, big.shape, pad, b.shape)))
            .collect())
    }


    #[allow(clippy::too_many_arguments)]
    fn two_rate(
        &self,
        params: &Params,
        b: &SampleBox,
        fields: &[&str],
        cancel: Option<&dyn Fn() -> bool>,
        boundary: &str,
        stride: usize,
        cont: &Continuation,
    ) -> GResult<BoxFields> {
        #[allow(clippy::cast_precision_loss)]
        let hc = b.h * stride as f64;
        let rc = self.blur_r(hc);
        let padc = rc.saturating_add(2);
        #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let nc: [usize; 3] = std::array::from_fn(|i| {
            (((b.shape[i] as f64 - 1.0) * b.h / hc).ceil() as usize).saturating_add(3)
        });
        let oc: [f64; 3] =
            std::array::from_fn(|c| b.origin[c] - hc * (b.axes[0][c] + b.axes[1][c] + b.axes[2][c]));
        let coarse = SampleBox { origin: oc, axes: b.axes, shape: nc, h: hc };
        let big = coarse.grown(padc);
        b.admit(MAX_PREVIEW_SAMPLES)?;
        big.admit(MAX_PREVIEW_SAMPLES)?;
        let ptsc = self.prep(&big, boundary);
        if cancel.is_some_and(|c| c()) {
            return Err(GeometryError::Cancelled);
        }
        let geff_big = self
            .run(params, &ptsc, big.shape, big.h, big.axes, rc, RunMode::Geff, cont, cancel)?
            .remove("geff")
            .unwrap_or_default();
        let geff_c = crop(&geff_big, big.shape, padc, nc);
        let mut geff = Vec::with_capacity(b.n());
        #[allow(clippy::cast_precision_loss)]
        for i in 0..b.shape[0] {
            for j in 0..b.shape[1] {
                for k in 0..b.shape[2] {
                    let idx = [i, j, k];
                    let ci: [f64; 3] = std::array::from_fn(|a| (idx[a] as f64 * b.h + hc) / hc);
                    geff.push(trilerp_np(&geff_c, nc, ci));
                }
            }
        }
        if cancel.is_some_and(|c| c()) {
            return Err(GeometryError::Cancelled);
        }
        let pts = self.prep(b, boundary);
        let fast = self.run(params, &pts, b.shape, b.h, b.axes, 0, RunMode::Fast, cont, cancel)?;
        Ok(self.combine(fast, &geff, fields, cont))
    }

    fn combine(&self, mut fast: BoxFields, geff: &[f64], fields: &[&str], cont: &Continuation) -> BoxFields {
        let w_iface = cont[4];
        let f = fast.get("f").cloned().unwrap_or_default();
        let nu = fast.get("nu").cloned().unwrap_or_default();
        let tau = fast.get("tau").cloned().unwrap_or_default();
        let eps = self.interface_eps();
        let n = f.len();
        let folded: Vec<f64> = f.iter().map(|x| (x * x + eps * eps).sqrt()).collect();
        let d: Vec<f64> = (0..n).map(|i| folded[i] / geff[i]).collect();
        let q: Vec<f64> = (0..n).map(|i| ((1.0 - nu[i]) * folded[i] + nu[i] * f[i]) / geff[i]).collect();
        let g: Vec<f64> = (0..n).map(|i| q[i] - tau[i]).collect();
        let rho_lat: Vec<f64> = g.iter().map(|x| 0.5 * ((-0.5 * x / w_iface).tanh() + 1.0)).collect();
        let mt = fast.get("mtilde").cloned().unwrap_or_default();
        fast.insert("rho".into(), mt.iter().zip(&rho_lat).map(|(a, b)| a * b).collect());
        fast.insert("q".into(), q);
        fast.insert("g".into(), g);
        fast.insert("d".into(), d);
        fast.insert("geff".into(), geff.to_vec());
        fast.insert("rho_lat".into(), rho_lat);
        fast.into_iter().filter(|(k, _)| fields.contains(&k.as_str())).collect()
    }
}

#[must_use]
pub fn crop(v: &[f64], shape: [usize; 3], pad: usize, inner: [usize; 3]) -> Vec<f64> {
    let mut out = Vec::with_capacity(inner.iter().product());
    let per = v.len() / (shape[0] * shape[1] * shape[2]).max(1);
    for c in 0..per {
        let base = c * shape[0] * shape[1] * shape[2];
        for i in 0..inner[0] {
            for j in 0..inner[1] {
                for k in 0..inner[2] {
                    out.push(v[base + ((i + pad) * shape[1] + j + pad) * shape[2] + k + pad]);
                }
            }
        }
    }
    out
}

#[must_use]
pub fn grad_world(field: &[f64], shape: [usize; 3], h: f64, a: [[f64; 3]; 3]) -> [Vec<f64>; 3] {
    let gl: [Vec<f64>; 3] =
        std::array::from_fn(|ax| crate::lattice::numerics::grad_center(field, shape, [h; 3], ax));
    let identity = (0..3).all(|r| {
        (0..3).all(|c| {
            (a[r][c] - if r == c { 1.0 } else { 0.0 }).abs() <= 1e-14 + 1e-5 * if r == c { 1.0 } else { 0.0 }
        })
    });
    if identity {
        return gl;
    }
    std::array::from_fn(|c| {
        use rayon::prelude::*;
        (0..field.len())
            .into_par_iter()
            .map(|i| gl[0][i] * a[0][c] + gl[1][i] * a[1][c] + gl[2][i] * a[2][c])
            .collect()
    })
}
