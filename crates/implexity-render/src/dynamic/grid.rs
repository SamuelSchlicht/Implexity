// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use crate::RenderError;

#[derive(Clone, Debug, PartialEq)]
pub struct GridField {
    pub shape: Vec<usize>,
    pub spacing: Vec<f64>,
    pub origin: Vec<f64>,
    pub components: usize,
    pub values: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Derived {
    Curl,
    QCriterion,
    Divergence,
}

impl Derived {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "curl" => Some(Self::Curl),
            "q_criterion" => Some(Self::QCriterion),
            "divergence" => Some(Self::Divergence),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Curl => "curl",
            Self::QCriterion => "q_criterion",
            Self::Divergence => "divergence",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reduce {
    Magnitude,
    Component(usize),
}

impl GridField {


    pub fn new(
        shape: Vec<usize>,
        spacing: Vec<f64>,
        origin: Vec<f64>,
        components: usize,
        values: Vec<f64>,
    ) -> Result<Self, RenderError> {
        let dims = shape.len();
        if !(2..=3).contains(&dims) || spacing.len() != dims || origin.len() != dims || components == 0 {
            return Err(RenderError::Invalid(
                "a grid field needs 2 or 3 axes with a spacing and origin per axis".into(),
            ));
        }
        if shape.iter().product::<usize>() * components != values.len() {
            return Err(RenderError::Invalid("grid field values do not match its shape".into()));
        }
        Ok(Self { shape, spacing, origin, components, values })
    }

    #[must_use]
    pub fn dims(&self) -> usize {
        self.shape.len()
    }

    #[must_use]
    pub fn bounds(&self) -> (Vec<f64>, Vec<f64>) {
        let lo = (0..self.dims()).map(|a| self.origin[a] - 0.5 * self.spacing[a]).collect();
        let hi = (0..self.dims())
            .map(|a| self.origin[a] + (self.shape[a] as f64 - 0.5) * self.spacing[a])
            .collect();
        (lo, hi)
    }

    fn cell_index(&self, idx: &[usize]) -> usize {
        idx.iter().zip(&self.shape).fold(0, |acc, (i, n)| acc * n + i)
    }

    #[must_use]
    pub fn sample(&self, p: &[f64]) -> Option<Vec<f64>> {
        let mut out = vec![0.0; self.components];
        self.sample_into(p, &mut out).then_some(out)
    }

    #[must_use]
    pub fn sample_into(&self, p: &[f64], out: &mut [f64]) -> bool {
        let d = self.dims();
        let c = self.components;
        if p.len() < d || out.len() < c {
            return false;
        }
        let mut base = [0usize; 3];
        let mut frac = [0.0f64; 3];
        for a in 0..d {
            let n = self.shape[a];
            let (lo, hi) =
                (self.origin[a] - 0.5 * self.spacing[a], self.origin[a] + (n as f64 - 0.5) * self.spacing[a]);
            if !(p[a] >= lo && p[a] <= hi) {
                return false;
            }
            let x = ((p[a] - self.origin[a]) / self.spacing[a]).clamp(0.0, (n - 1) as f64);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let i = (x.floor() as usize).min(n.saturating_sub(2));
            base[a] = i;
            frac[a] = if n > 1 { x - i as f64 } else { 0.0 };
        }
        let out = &mut out[..c];
        out.fill(0.0);
        for corner in 0..(1usize << d) {
            let mut w = 1.0;
            let mut cell = 0usize;
            for a in 0..d {
                let bit = (corner >> a) & 1;
                let n = self.shape[a];
                cell = cell * n + (base[a] + bit).min(n - 1);
                w *= if bit == 1 { frac[a] } else { 1.0 - frac[a] };
            }
            if w == 0.0 {
                continue;
            }
            for (o, v) in out.iter_mut().zip(&self.values[cell * c..(cell + 1) * c]) {
                if !v.is_finite() {
                    return false;
                }
                *o += w * v;
            }
        }
        true
    }

    #[must_use]
    pub fn sample2(&self, p: &[f64]) -> Option<[f64; 2]> {
        let mut buf = [0.0f64; 4];
        if self.components <= 4 {
            return (self.components >= 2 && self.sample_into(p, &mut buf)).then_some([buf[0], buf[1]]);
        }
        self.sample(p).map(|v| [v[0], v[1]])
    }

    #[must_use]
    pub fn sample_scalar(&self, p: &[f64], reduce: Reduce) -> Option<f64> {
        let reduce_of = |v: &[f64]| match reduce {
            Reduce::Magnitude if v.len() > 1 => Some(v.iter().map(|x| x * x).sum::<f64>().sqrt()),
            Reduce::Magnitude => Some(v[0]),
            Reduce::Component(k) => v.get(k).copied(),
        };
        let c = self.components;
        if c <= 4 {
            let mut buf = [0.0f64; 4];
            return if self.sample_into(p, &mut buf) { reduce_of(&buf[..c]) } else { None };
        }
        reduce_of(&self.sample(p)?)
    }



    pub fn slice(&self, axis: usize, at: f64) -> Result<Self, RenderError> {
        if self.dims() == 2 {
            return Ok(self.clone());
        }
        if axis > 2 {
            return Err(RenderError::Invalid("slice axis must be 0, 1 or 2".into()));
        }
        let n = self.shape[axis];
        let x = ((at - self.origin[axis]) / self.spacing[axis]).clamp(0.0, (n - 1) as f64);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i0 = (x.floor() as usize).min(n.saturating_sub(1));
        let i1 = (i0 + 1).min(n - 1);
        let f = x - i0 as f64;
        let keep: Vec<usize> = (0..3).filter(|&a| a != axis).collect();
        let (nu, nv) = (self.shape[keep[0]], self.shape[keep[1]]);
        let c = self.components;
        let mut values = Vec::with_capacity(nu * nv * c);
        for u in 0..nu {
            for v in 0..nv {
                let mut idx0 = [0usize; 3];
                idx0[keep[0]] = u;
                idx0[keep[1]] = v;
                let mut idx1 = idx0;
                idx0[axis] = i0;
                idx1[axis] = i1;
                let (a, b) = (self.cell_index(&idx0), self.cell_index(&idx1));
                for k in 0..c {
                    values.push((1.0 - f) * self.values[a * c + k] + f * self.values[b * c + k]);
                }
            }
        }
        Self::new(
            vec![nu, nv],
            keep.iter().map(|&a| self.spacing[a]).collect(),
            keep.iter().map(|&a| self.origin[a]).collect(),
            c,
            values,
        )
    }

    #[must_use]
    pub fn in_plane(&self, axis: Option<usize>) -> Self {
        if self.components < 3 || axis.is_none() {
            return self.clone();
        }
        let axis = axis.unwrap_or(2);
        let keep: Vec<usize> = (0..3).filter(|&a| a != axis).collect();
        let values = self.values.chunks_exact(3).flat_map(|v| [v[keep[0]], v[keep[1]]]).collect();
        Self {
            shape: self.shape.clone(),
            spacing: self.spacing.clone(),
            origin: self.origin.clone(),
            components: 2,
            values,
        }
    }

    #[must_use]
    pub fn line(&self, p0: &[f64], p1: &[f64], n: usize, reduce: Reduce) -> Vec<f64> {
        (0..n)
            .map(|k| {
                let s = if n > 1 { k as f64 / (n - 1) as f64 } else { 0.5 };
                let p: Vec<f64> = p0.iter().zip(p1).map(|(a, b)| a + s * (b - a)).collect();
                self.sample_scalar(&p, reduce).unwrap_or(f64::NAN)
            })
            .collect()
    }

    fn gradient(&self, c: usize, a: usize) -> Vec<f64> {
        let d = self.dims();
        let n = self.shape[a];
        let stride: usize = self.shape[a + 1..d].iter().product();
        let cells: usize = self.shape.iter().product();
        let h = self.spacing[a];
        let k = self.components;
        (0..cells)
            .map(|cell| {
                let i = (cell / stride) % n;
                if n < 2 {
                    return 0.0;
                }
                let at = |j: usize| self.values[(cell - i * stride + j * stride) * k + c];
                if i == 0 {
                    (at(1) - at(0)) / h
                } else if i == n - 1 {
                    (at(n - 1) - at(n - 2)) / h
                } else {
                    (at(i + 1) - at(i - 1)) / (2.0 * h)
                }
            })
            .collect()
    }



    #[allow(clippy::needless_range_loop)]
    pub fn derive(&self, op: Derived) -> Result<Self, RenderError> {
        let d = self.dims();
        if self.components != d {
            return Err(RenderError::Invalid(format!(
                "{} needs a vector field with {d} components on this {d}-D grid",
                op.name()
            )));
        }
        let cells: usize = self.shape.iter().product();
        let g: Vec<Vec<Vec<f64>>> = (0..d).map(|c| (0..d).map(|a| self.gradient(c, a)).collect()).collect();
        let (components, values): (usize, Vec<f64>) = match op {
            Derived::Divergence => (1, (0..cells).map(|i| (0..d).map(|a| g[a][a][i]).sum()).collect()),
            Derived::Curl if d == 2 => (1, (0..cells).map(|i| g[1][0][i] - g[0][1][i]).collect()),
            Derived::Curl => (
                3,
                (0..cells)
                    .flat_map(|i| [g[2][1][i] - g[1][2][i], g[0][2][i] - g[2][0][i], g[1][0][i] - g[0][1][i]])
                    .collect(),
            ),
            Derived::QCriterion => (
                1,
                (0..cells)
                    .map(|i| {

                        let mut q = 0.0;
                        for a in 0..d {
                            for b in 0..d {
                                let (gab, gba) = (g[a][b][i], g[b][a][i]);
                                let (sym, anti) = (0.5 * (gab + gba), 0.5 * (gab - gba));
                                q += anti * anti - sym * sym;
                            }
                        }
                        0.5 * q
                    })
                    .collect(),
            ),
        };
        Self::new(self.shape.clone(), self.spacing.clone(), self.origin.clone(), components, values)
    }

    #[must_use]
    pub fn range(&self, reduce: Reduce) -> Option<(f64, f64)> {
        let c = self.components;
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for v in self.values.chunks_exact(c) {
            let x = match reduce {
                Reduce::Magnitude if c > 1 => v.iter().map(|a| a * a).sum::<f64>().sqrt(),
                Reduce::Magnitude => v[0],
                Reduce::Component(k) => v.get(k).copied().unwrap_or(f64::NAN),
            };
            if x.is_finite() {
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
        (lo <= hi).then_some((lo, hi))
    }
}

