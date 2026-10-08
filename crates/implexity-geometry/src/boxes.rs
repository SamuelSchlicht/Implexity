// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use crate::error::{GResult, value_err};

pub const MM: f64 = 1.0e-3;

#[must_use]
pub fn quantise_count(n: i64, q: i64) -> i64 {
    if n <= q {
        return q;
    }

    (n.saturating_add(q - 1) / q).saturating_mul(q)
}

pub const MAX_PREVIEW_SAMPLES: usize = 1 << 27;


#[derive(Clone, Debug, PartialEq)]
pub struct SampleBox {
    pub origin: [f64; 3],
    pub axes: [[f64; 3]; 3],
    pub shape: [usize; 3],
    pub h: f64,
}

impl SampleBox {
    #[must_use]
    pub fn n(&self) -> usize {
        self.shape[0].saturating_mul(self.shape[1]).saturating_mul(self.shape[2])
    }

    #[must_use]
    pub fn checked_n(&self) -> Option<usize> {
        self.shape[0].checked_mul(self.shape[1])?.checked_mul(self.shape[2])
    }


    pub fn admit(&self, max: usize) -> GResult<usize> {
        match self.checked_n() {
            Some(n) if n <= max => Ok(n),
            _ => value_err(format!(
                "sample box {}x{}x{} exceeds the {max}-sample preview bound; reduce the extent or \
                 coarsen the level of detail",
                self.shape[0], self.shape[1], self.shape[2]
            )),
        }
    }

    #[must_use]
    pub fn axis_aligned(&self) -> bool {
        for (r, row) in self.axes.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let want = if r == c { 1.0 } else { 0.0 };
                if (v - want).abs() > 1e-12 + 1e-5 * f64::abs(want) {
                    return false;
                }
            }
        }
        true
    }

    #[must_use]
    pub fn grown(&self, pad: usize) -> Self {
        if pad == 0 {
            return self.clone();
        }
        #[allow(clippy::cast_precision_loss)]
        let p = pad as f64;
        let mut o = self.origin;
        for (c, oc) in o.iter_mut().enumerate() {
            let s = self.axes[0][c] + self.axes[1][c] + self.axes[2][c];
            *oc -= p * self.h * s;
        }
        Self {
            origin: o,
            axes: self.axes,
            shape: [
                self.shape[0].saturating_add(2 * pad),
                self.shape[1].saturating_add(2 * pad),
                self.shape[2].saturating_add(2 * pad),
            ],
            h: self.h,
        }
    }

    #[must_use]
    pub fn points(&self) -> Vec<[f64; 3]> {
        let [nx, ny, nz] = self.shape;
        let a = &self.axes;
        let o = self.origin;
        let mut out = Vec::with_capacity(self.n());
        for i in 0..nx {
            #[allow(clippy::cast_precision_loss)]
            let ih = i as f64 * self.h;
            for j in 0..ny {
                #[allow(clippy::cast_precision_loss)]
                let jh = j as f64 * self.h;
                for k in 0..nz {
                    #[allow(clippy::cast_precision_loss)]
                    let kh = k as f64 * self.h;
                    let mut p = [0.0; 3];
                    for c in 0..3 {
                        p[c] = o[c] + ih * a[0][c] + jh * a[1][c] + kh * a[2][c];
                    }
                    out.push(p);
                }
            }
        }
        out
    }

    #[must_use]
    pub fn points_at_mm(&self, idx: &[[f64; 3]]) -> Vec<[f64; 3]> {
        let a = &self.axes;
        let o = self.origin;
        idx.iter()
            .map(|[i, j, k]| {
                let (ih, jh, kh) = (i * self.h, j * self.h, k * self.h);
                let mut p = [0.0; 3];
                for c in 0..3 {
                    p[c] = (o[c] + ih * a[0][c] + jh * a[1][c] + kh * a[2][c]) * 1000.0;
                }
                p
            })
            .collect()
    }

    #[must_use]
    pub fn points_mm(&self) -> Vec<[f64; 3]> {
        self.points().into_iter().map(|p| [p[0] * 1000.0, p[1] * 1000.0, p[2] * 1000.0]).collect()
    }
}

fn norm3(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn round_py(x: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let r = x.round_ties_even() as i64;
    r
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn plane_box(
    centre_mm: [f64; 3],
    normal: [f64; 3],
    up_hint: [f64; 3],
    width_mm: f64,
    height_mm: f64,
    h_mm: f64,
    thickness_samples: i64,
    quant: i64,
) -> (SampleBox, [f64; 3], [f64; 3]) {
    let n = scale(normal, 1.0 / norm3(normal));
    let mut up = up_hint;
    let un = norm3(up);
    if un < 1e-12 || dot(scale(up, 1.0 / un), n).abs() > 0.98 {
        up = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    }
    let mut u = cross(up, n);
    u = scale(u, 1.0 / norm3(u));
    let mut v = cross(n, u);
    v = scale(v, 1.0 / norm3(v));
    let h = h_mm * MM;
    let nu = quantise_count(round_py(width_mm * MM / h).saturating_add(1), quant);
    let nv = quantise_count(round_py(height_mm * MM / h).saturating_add(1), quant);
    let nt = thickness_samples.max(1);
    #[allow(clippy::cast_precision_loss)]
    let h = ((width_mm * MM) / (nu - 1).max(1) as f64).min((height_mm * MM) / (nv - 1).max(1) as f64);
    let c = scale(centre_mm, MM);
    let mut o = [0.0; 3];
    for k in 0..3 {
        #[allow(clippy::cast_precision_loss)]
        {
            o[k] = c[k]
                - 0.5 * (nu - 1) as f64 * h * u[k]
                - 0.5 * (nv - 1) as f64 * h * v[k]
                - 0.5 * (nt - 1) as f64 * h * n[k];
        }
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let shape = [nu as usize, nv as usize, nt as usize];
    (SampleBox { origin: o, axes: [u, v, n], shape, h }, u, v)
}


pub fn slab_box(
    centre_mm: [f64; 3],
    axes3: [[f64; 3]; 3],
    extent_mm: [f64; 3],
    h_mm: Option<f64>,
    samples: Option<&[i64]>,
    quant: i64,
) -> GResult<SampleBox> {
    let mut a = axes3;
    for i in 0..3 {
        for j in 0..i {
            let d = dot(a[i], a[j]);
            for c in 0..3 {
                a[i][c] -= d * a[j][c];
            }
        }
        let nrm = norm3(a[i]);
        if nrm < 1e-12 {
            return value_err("degenerate slab frame");
        }
        for c in 0..3 {
            a[i][c] /= nrm;
        }
    }
    let ext = [extent_mm[0] * MM, extent_mm[1] * MM, extent_mm[2] * MM];
    let (ns, h) = if let Some(req) = samples {
        let ns: [i64; 3] = if req.len() >= 3 {
            [quantise_count(req[0], quant), quantise_count(req[1], quant), quantise_count(req[2], quant)]
        } else {
            let n_uv = quantise_count(req.first().copied().unwrap_or(1), quant);
            #[allow(clippy::cast_precision_loss)]
            let h0 = ext[0].max(ext[1]) / (n_uv - 1).max(1) as f64;
            [
                quantise_count(round_py(ext[0] / h0).saturating_add(1), quant),
                quantise_count(round_py(ext[1] / h0).saturating_add(1), quant),
                quantise_count(round_py(ext[2] / h0).saturating_add(1), quant),
            ]
        };
        #[allow(clippy::cast_precision_loss)]
        let h = (0..3).map(|i| ext[i] / (ns[i] - 1).max(1) as f64).fold(f64::NEG_INFINITY, f64::max);
        (ns, h)
    } else {
        let h = h_mm.unwrap_or(1.0) * MM;
        let ns = [
            quantise_count(round_py(ext[0] / h).saturating_add(1), quant),
            quantise_count(round_py(ext[1] / h).saturating_add(1), quant),
            quantise_count(round_py(ext[2] / h).saturating_add(1), quant),
        ];
        #[allow(clippy::cast_precision_loss)]
        let h = (0..3).map(|i| ext[i] / (ns[i] - 1).max(1) as f64).fold(f64::INFINITY, f64::min);
        (ns, h)
    };
    let c = scale(centre_mm, MM);
    let mut acc = [0.0; 3];
    for i in 0..3 {
        #[allow(clippy::cast_precision_loss)]
        let s = (ns[i] - 1) as f64 * h;
        for k in 0..3 {
            acc[k] += s * a[i][k];
        }
    }
    let o = [c[0] - 0.5 * acc[0], c[1] - 0.5 * acc[1], c[2] - 0.5 * acc[2]];
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let shape = [ns[0] as usize, ns[1] as usize, ns[2] as usize];
    Ok(SampleBox { origin: o, axes: a, shape, h })
}

