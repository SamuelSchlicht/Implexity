// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::{f64::consts::PI,path::Path};
use implexity_ad::Scalar;
use serde_json::{Map,Value,json};
#[derive(Debug,Clone,PartialEq,thiserror::Error)]
pub enum FieldProcessingError {
    #[error("{message}")]
    Validation {                   
        message:String,                
        path:String,             
        details:Map<String,Value> },
    #[error("{0}")]
    Io(String),
}
impl FieldProcessingError {
 fn validation(message:impl Into<String>,path:impl Into<String>)->Self{Self::Validation{message:message.into(),path:path.into(),details:Map::new()}}
 fn detail(mut self,key:&str,value:Value)->Self{if let Self::Validation{details,..}=&mut self{details.insert(key.into(),value);}self}
}
type PResult<T>=Result<T,FieldProcessingError>;
fn check_positive_finite(name: &str, value: f64) -> PResult<()> {
    if !value.is_finite() || value <= 0.0 {
        return Err(FieldProcessingError::validation(
            format!("{name} must be finite and > 0"),
            format!("postprocessing.{name}"),
        )
        .detail("value", json!(value)));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Field2<S> {
    D1(Vec<S>),
    D2(Vec<Vec<S>>),
}

#[must_use]
pub fn gaussian_kernel_1d(sigma: f64) -> Vec<f64> {
    #[allow(clippy::cast_possible_truncation)]
    let radius = ((4.0 * sigma).ceil() as i64).max(1);
    #[allow(clippy::cast_precision_loss)]
    let w: Vec<f64> = (-radius..=radius).map(|x| (-0.5 * (x as f64 / sigma).powi(2)).exp()).collect();
    let total: f64 = w.iter().sum();
    w.iter().map(|x| x / total).collect()
}

pub fn convolve_valid<S: Scalar>(a: &[S], v: &[f64]) -> Vec<S> {
    if a.len() >= v.len() {
        let m = v.len();
        (0..=a.len() - m)
            .map(|k| {
                let mut s = S::zero();
                for j in 0..m {
                    s += a[k + j] * v[m - 1 - j];
                }
                s
            })
            .collect()
    } else {
        let m = a.len();
        (0..=v.len() - m)
            .map(|k| {
                let mut s = S::zero();
                for j in 0..m {
                    s += a[m - 1 - j] * v[k + j];
                }
                s
            })
            .collect()
    }
}

pub fn conv1d_reflect<S: Scalar>(field: &[S], kernel: &[f64]) -> Vec<S> {
    let n = field.len();
    let pad = kernel.len() / 2;
    if n < 2 {
        return field.to_vec();
    }
    let padded: Vec<S> = if pad > 0 {
        let left: Vec<S> = field[1.min(n)..(pad + 1).min(n)].iter().rev().copied().collect();
        let start = n.saturating_sub(pad + 1);
        let right: Vec<S> = field[start..n - 1].iter().rev().copied().collect();
        left.into_iter().chain(field.iter().copied()).chain(right).collect()
    } else {
        field.to_vec()
    };
    convolve_valid(&padded, kernel)
}


pub fn smooth_gaussian<S: Scalar>(field: &Field2<S>, sigma_cells: f64) -> PResult<Field2<S>> {
    check_positive_finite("sigma_cells", sigma_cells)?;
    let kernel = gaussian_kernel_1d(sigma_cells);
    Ok(match field {
        Field2::D1(v) => Field2::D1(conv1d_reflect(v, &kernel)),
        Field2::D2(rows) => {
            let rows: Vec<Vec<S>> = rows.iter().map(|r| conv1d_reflect(r, &kernel)).collect();
            let ncol = rows.first().map_or(0, Vec::len);
            let cols: Vec<Vec<S>> = (0..ncol)
                .map(|c| conv1d_reflect(&rows.iter().map(|r| r[c]).collect::<Vec<_>>(), &kernel))
                .collect();
            let nrow = cols.first().map_or(0, Vec::len);
            Field2::D2((0..nrow).map(|r| cols.iter().map(|c| c[r]).collect()).collect())
        }
    })
}


pub fn smooth_helmholtz<S: Scalar>(
    field: &Field2<S>,
    filter_radius_m: f64,
    cell_size_m: f64,
    n_iterations: usize,
    tolerance: f64,
) -> PResult<Field2<S>> {
    check_positive_finite("filter_radius_m", filter_radius_m)?;
    check_positive_finite("cell_size_m", cell_size_m)?;
    let coeff = (filter_radius_m / cell_size_m).powi(2);
    Ok(match field {
        Field2::D1(u) => {
            let n = u.len();
            let mut x = u.clone();
            for _ in 0..n_iterations {
                let new: Vec<S> = (0..n)
                    .map(|k| {
                        let l = x[k.saturating_sub(1)];
                        let r = x[(k + 1).min(n - 1)];
                        (u[k] + (l + r) * coeff) / (1.0 + coeff * 2.0)
                    })
                    .collect();
                let delta =
                    new.iter().zip(&x).map(|(a, b)| (a.value() - b.value()).abs()).fold(0.0, f64::max);
                x = new;
                if delta < tolerance {
                    break;
                }
            }
            Field2::D1(x)
        }
        Field2::D2(u) => {
            let (nr, nc) = (u.len(), u.first().map_or(0, Vec::len));
            let mut x = u.clone();
            for _ in 0..n_iterations {
                let new: Vec<Vec<S>> = (0..nr)
                    .map(|i| {
                        (0..nc)
                            .map(|j| {
                                let up = x[i.saturating_sub(1)][j];
                                let down = x[(i + 1).min(nr - 1)][j];
                                let left = x[i][j.saturating_sub(1)];
                                let right = x[i][(j + 1).min(nc - 1)];
                                (u[i][j] + (up + down + left + right) * coeff) / (1.0 + coeff * 4.0)
                            })
                            .collect()
                    })
                    .collect();
                let delta = new
                    .iter()
                    .flatten()
                    .zip(x.iter().flatten())
                    .map(|(a, b)| (a.value() - b.value()).abs())
                    .fold(0.0, f64::max);
                x = new;
                if delta < tolerance {
                    break;
                }
            }
            Field2::D2(x)
        }
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field3<S> {
    pub shape: [usize; 3],
    pub values: Vec<S>,
}

fn mean_of<S: Scalar>(v: &[S]) -> S {
    let mut s = S::zero();
    for x in v {
        s += *x;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = v.len() as f64;
    s / n
}


pub fn axisymmetric_average_2d<S: Scalar>(rows: &[Vec<S>], n_azimuthal: usize) -> PResult<Vec<S>> {
    check_azimuthal(n_azimuthal, 3, "axisymmetric_average", "n_azimuthal must be an int >= 3")?;
    Ok(rows.iter().map(|r| mean_of(r)).collect())
}

fn check_azimuthal(n: usize, min: usize, op: &str, message: &str) -> PResult<()> {
    if n < min {
        return Err(FieldProcessingError::validation(message, format!("postprocessing.{op}.n_azimuthal"))
            .detail("value", json!(n)));
    }
    Ok(())
}


pub fn axisymmetric_average_3d<S: Scalar>(
    field: &Field3<S>,
    n_azimuthal: usize,
    axis: usize,
) -> PResult<Vec<Vec<S>>> {
    check_azimuthal(n_azimuthal, 3, "axisymmetric_average", "n_azimuthal must be an int >= 3")?;
    if axis > 2 {
        return Err(FieldProcessingError::validation(
            "axis must be 0, 1, or 2",
            "postprocessing.axisymmetric_average.axis",
        )
        .detail("axis", json!(axis)));
    }
    let arr = field.move_axis_last(axis);
    let [nx, ny, nz] = arr.shape;
    #[allow(clippy::cast_precision_loss)]
    let r_max = (nx.min(ny) as f64 - 1.0) * 0.5;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n_r = ((r_max.floor() as usize) + 1).max(2);
    #[allow(clippy::cast_precision_loss)]
    let (cx, cy) = ((nx as f64 - 1.0) * 0.5, (ny as f64 - 1.0) * 0.5);
    let rs = crate::numpy::linspace(0.0, r_max, n_r);
    #[allow(clippy::cast_precision_loss)]
    let phis: Vec<f64> = (0..n_azimuthal).map(|k| k as f64 * (2.0 * PI / n_azimuthal as f64)).collect();
    #[allow(clippy::cast_precision_loss)]
    let (xmax, ymax) = ((nx - 1) as f64, (ny - 1) as f64);
    let mut out = Vec::with_capacity(n_r);
    for r in &rs {
        let mut acc = vec![S::zero(); nz];
        for phi in &phis {
            let xs = (cx + r * phi.cos()).clamp(0.0, xmax);
            let ys = (cy + r * phi.sin()).clamp(0.0, ymax);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (x0, y0) = (xs.floor() as usize, ys.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(nx - 1), (y0 + 1).min(ny - 1));
            #[allow(clippy::cast_precision_loss)]
            let (wx, wy) = (xs - x0 as f64, ys - y0 as f64);
            for (k, a) in acc.iter_mut().enumerate() {
                *a += arr.at(x0, y0, k) * (1.0 - wx) * (1.0 - wy)
                    + arr.at(x0, y1, k) * (1.0 - wx) * wy
                    + arr.at(x1, y0, k) * wx * (1.0 - wy)
                    + arr.at(x1, y1, k) * wx * wy;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        out.push(acc.into_iter().map(|a| a / n_azimuthal as f64).collect());
    }
    Ok(out)
}

fn dft_amplitudes(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    #[allow(clippy::cast_precision_loss)]
    (0..n)
        .map(|k| {
            let (mut re, mut im) = (0.0, 0.0);
            for (m, v) in x.iter().enumerate() {
                let ang = -2.0 * PI * ((k * m) % n) as f64 / n as f64;
                re += v * ang.cos();
                im += v * ang.sin();
            }
            re.hypot(im)
        })
        .collect()
}


pub fn detect_n_fold_symmetry(series: &[Vec<f64>], n_azimuthal: usize) -> PResult<usize> {
    check_azimuthal(
        n_azimuthal,
        4,
        "detect_n_fold_symmetry",
        "n_azimuthal must be an int >= 4 for periodicity detection",
    )?;
    if series.iter().any(|s| s.len() != n_azimuthal) {
        return Err(FieldProcessingError::validation(
            "field.shape[1] must equal n_azimuthal (phi axis is axis=1)",
            "postprocessing.detect_n_fold_symmetry",
        ));
    }
    let mut power = vec![0.0; n_azimuthal];
    for s in series {
        for (p, a) in power.iter_mut().zip(dft_amplitudes(s)) {
            *p += a;
        }
    }
    let half = n_azimuthal / 2;
    let mut best = 1;
    for k in 1..=half {
        if power[k] > power[best] {
            best = k;
        }
    }
    Ok(best)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AzimuthalFilter {
    Fft {
        keep_harmonics: usize,
    },
    Gaussian {
        sigma_cells: f64,
    },
    Constant,
}


pub fn axisymmetric_fold_then_filter<S: Scalar>(
    series: &[Vec<S>],
    n_azimuthal: usize,
    n_fold_hint: Option<i64>,
    filter: AzimuthalFilter,
) -> PResult<Vec<Vec<S>>> {
    check_azimuthal(n_azimuthal, 4, "axisymmetric_fold_then_filter", "n_azimuthal must be an int >= 4")?;
    if series.iter().any(|s| s.len() != n_azimuthal) {
        return Err(FieldProcessingError::validation(
            "field.shape[1] must equal n_azimuthal",
            "postprocessing.axisymmetric_fold_then_filter",
        ));
    }
    let mut n_fold = if let Some(h) = n_fold_hint {
        h
    } else {
        let values: Vec<Vec<f64>> = series.iter().map(|s| s.iter().map(Scalar::value).collect()).collect();
        i64::try_from(detect_n_fold_symmetry(&values, n_azimuthal)?).unwrap_or(1)
    }
    .max(1);
    let n_az = i64::try_from(n_azimuthal).unwrap_or(i64::MAX);
    while n_fold > 1 && n_az % n_fold != 0 {
        n_fold -= 1;
    }
    let n_fold = usize::try_from(n_fold).unwrap_or(1);
    let n_sector = n_azimuthal / n_fold;
    let fold = |s: &[S]| -> Vec<S> {
        (0..n_sector)
            .map(|j| mean_of(&(0..n_fold).map(|f| s[f * n_sector + j]).collect::<Vec<_>>()))
            .collect()
    };
    let filtered: Vec<Vec<S>> = series
        .iter()
        .map(|s| {
            let folded = fold(s);
            match filter {
                AzimuthalFilter::Constant => vec![mean_of(&folded); n_sector],
                AzimuthalFilter::Fft { keep_harmonics } => {
                    let n_keep = keep_harmonics.min(n_sector / 2).max(1);
                    let mut mask = vec![0.0; n_sector];
                    mask[0] = 1.0;
                    for k in 1..=n_keep {
                        mask[k] = 1.0;
                        mask[n_sector - k] = 1.0;
                    }
                    #[allow(clippy::cast_precision_loss)]
                    let kernel: Vec<f64> = (0..n_sector)
                        .map(|d| {
                            mask.iter()
                                .enumerate()
                                .map(|(k, m)| {
                                    m * (2.0 * PI * ((k * d) % n_sector) as f64 / n_sector as f64).cos()
                                })
                                .sum::<f64>()
                                / n_sector as f64
                        })
                        .collect();
                    (0..n_sector)
                        .map(|j| {
                            let mut acc = S::zero();
                            for (m, x) in folded.iter().enumerate() {
                                acc += *x * kernel[(j + n_sector - m) % n_sector];
                            }
                            acc
                        })
                        .collect()
                }
                AzimuthalFilter::Gaussian { sigma_cells } => {
                    let sigma = sigma_cells.max(1.0e-6);
                    let kern = gaussian_kernel_1d(sigma);
                    let radius = kern.len() / 2;
                    let len = folded.len();
                    let padded: Vec<S> = folded[len.saturating_sub(radius)..]
                        .iter()
                        .chain(folded.iter())
                        .chain(folded[..radius.min(len)].iter())
                        .copied()
                        .collect();
                    convolve_valid(&padded, &kern)
                }
            }
        })
        .collect();
    Ok(filtered.into_iter().map(|f| (0..n_fold).flat_map(|_| f.iter().copied()).collect()).collect())
}


pub fn axisymmetric_extrude<S: Scalar>(
    profile: &[Vec<S>],
    n_azimuthal: usize,
    cell_size: f64,
    n_transverse: Option<usize>,
) -> PResult<Field3<S>> {
    check_positive_finite("cell_size", cell_size)?;
    check_azimuthal(n_azimuthal, 3, "axisymmetric_extrude", "n_azimuthal must be an int >= 3")?;
    let n_r = profile.len();
    let n_z = profile.first().map_or(0, Vec::len);
    let n_xy = n_transverse.unwrap_or((2 * n_r).saturating_sub(1));
    if n_xy < 3 {
        return Err(FieldProcessingError::validation(
            "n_transverse must be >= 3",
            "postprocessing.axisymmetric_extrude.n_transverse",
        )
        .detail("value", json!(n_xy)));
    }
    #[allow(clippy::cast_precision_loss)]
    let c = (n_xy as f64 - 1.0) * 0.5;
    #[allow(clippy::cast_precision_loss)]
    let r_max = n_r as f64 - 1.0;
    let mut values = Vec::with_capacity(n_xy * n_xy * n_z);
    for i in 0..n_xy {
        for j in 0..n_xy {
            #[allow(clippy::cast_precision_loss)]
            let (xg, yg) = (i as f64 - c, j as f64 - c);
            let r = (xg * xg + yg * yg).sqrt().clamp(0.0, r_max);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let r0 = r.floor() as usize;
            let r1 = (r0 + 1).min(n_r - 1);
            #[allow(clippy::cast_precision_loss)]
            let w = r - r0 as f64;
            for (a, b) in profile[r0].iter().zip(&profile[r1]).take(n_z) {
                values.push(*a * (1.0 - w) + *b * w);
            }
        }
    }
    Ok(Field3 { shape: [n_xy, n_xy, n_z], values })
}

const CUBE_TO_TETS: [[usize; 4]; 6] =
    [[0, 5, 1, 6], [0, 1, 2, 6], [0, 2, 3, 6], [0, 3, 7, 6], [0, 7, 4, 6], [0, 4, 5, 6]];

const CORNERS: [[usize; 3]; 8] =
    [[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0], [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]];

type Tri = [[f64; 3]; 3];

fn tet_triangles(values: [f64; 4], positions: [[f64; 3]; 4], iso: f64, out: &mut Vec<Tri>) {
    let inside: Vec<bool> = values.iter().map(|v| v - iso < 0.0).collect();
    let n_in = inside.iter().filter(|b| **b).count();
    if n_in == 0 || n_in == 4 {
        return;
    }
    let edge = |a: usize, b: usize| -> [f64; 3] {
        let (va, vb) = (values[a] - iso, values[b] - iso);
        let denom = va - vb;
        let t = if denom.abs() < 1.0e-300 { 0.5 } else { (va / denom).clamp(0.0, 1.0) };
        std::array::from_fn(|c| positions[a][c] * (1.0 - t) + positions[b][c] * t)
    };
    let ins: Vec<usize> = (0..4).filter(|i| inside[*i]).collect();
    let outs: Vec<usize> = (0..4).filter(|i| !inside[*i]).collect();
    if n_in == 1 || n_in == 3 {
        let (lone, others) = if n_in == 1 { (ins[0], &outs) } else { (outs[0], &ins) };
        out.push([edge(lone, others[0]), edge(lone, others[1]), edge(lone, others[2])]);
    } else {
        let (a, b, c, d) = (ins[0], ins[1], outs[0], outs[1]);
        let (ac, ad, bc, bd) = (edge(a, c), edge(a, d), edge(b, c), edge(b, d));
        out.push([ac, bc, bd]);
        out.push([ac, bd, ad]);
    }
}

#[must_use]
pub fn marching_tets(sdf: &Field3<f64>, iso: f64, spacing: [f64; 3]) -> Vec<Tri> {
    let [nx, ny, nz] = sdf.shape;
    let mut tris = Vec::new();
    if nx < 2 || ny < 2 || nz < 2 {
        return tris;
    }
    for i in 0..nx - 1 {
        for j in 0..ny - 1 {
            for k in 0..nz - 1 {
                let mut vals = [0.0; 8];
                let mut pos = [[0.0; 3]; 8];
                for (c, off) in CORNERS.iter().enumerate() {
                    let (ci, cj, ck) = (i + off[0], j + off[1], k + off[2]);
                    vals[c] = sdf.at(ci, cj, ck);
                    #[allow(clippy::cast_precision_loss)]
                    let p = [ci as f64 * spacing[0], cj as f64 * spacing[1], ck as f64 * spacing[2]];
                    pos[c] = p;
                }
                if vals.iter().all(|v| *v < iso) || vals.iter().all(|v| *v >= iso) {
                    continue;
                }
                for tet in CUBE_TO_TETS {
                    tet_triangles(tet.map(|t| vals[t]), tet.map(|t| pos[t]), iso, &mut tris);
                }
            }
        }
    }
    tris
}

#[must_use]
pub fn binary_stl(triangles: &[Tri]) -> Vec<u8> {
    let mut header = b"IMPLEXITY-POSTPROCESSING STL export".to_vec();
    header.resize(80, b' ');
    let mut out = header;
    out.extend_from_slice(&u32::try_from(triangles.len()).unwrap_or(u32::MAX).to_le_bytes());
    for t in triangles {
        let (a, b) = (
            std::array::from_fn::<f64, 3, _>(|c| t[1][c] - t[0][c]),
            std::array::from_fn::<f64, 3, _>(|c| t[2][c] - t[0][c]),
        );
        let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        let norm = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let n = if norm > 0.0 { n.map(|x| x / norm) } else { [0.0; 3] };
        for v in [n, t[0], t[1], t[2]] {
            for x in v {
                #[allow(clippy::cast_possible_truncation)]
                out.extend_from_slice(&(x as f32).to_le_bytes());
            }
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    out
}


pub fn export_stl(sdf: &Field3<f64>, resolution: f64, output_path: &Path, iso: f64) -> PResult<Value> {
    check_positive_finite("resolution", resolution)?;
    let tris = marching_tets(sdf, iso, [resolution; 3]);
    std::fs::write(output_path, binary_stl(&tris)).map_err(|e| FieldProcessingError::Io(e.to_string()))?;
    Ok(json!({"n_triangles": tris.len(), "path": output_path.to_string_lossy(), "format": "stl-binary"}))
}


impl<S: Copy> Field3<S> {
    fn at(&self, i: usize, j: usize, k: usize) -> S {
        self.values[(i * self.shape[1] + j) * self.shape[2] + k]
    }

    #[must_use]
    pub fn move_axis_last(&self, axis: usize) -> Self {
        let order: [usize; 3] = match axis {
            0 => [1, 2, 0],
            1 => [0, 2, 1],
            _ => [0, 1, 2],
        };
        let shape = order.map(|a| self.shape[a]);
        let mut values = Vec::with_capacity(self.values.len());
        for a in 0..shape[0] {
            for b in 0..shape[1] {
                for c in 0..shape[2] {
                    let mut idx = [0; 3];
                    idx[order[0]] = a;
                    idx[order[1]] = b;
                    idx[order[2]] = c;
                    values.push(self.at(idx[0], idx[1], idx[2]));
                }
            }
        }
        Self { shape, values }
    }
}