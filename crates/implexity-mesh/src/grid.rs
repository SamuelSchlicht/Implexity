// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use crate::MeshError;

#[derive(Clone, Copy, Debug)]
pub struct Field3<'a> {
    pub shape: [usize; 3],
    pub data: &'a [f64],
}

impl<'a> Field3<'a> {


    pub fn new(shape: [usize; 3], data: &'a [f64]) -> Result<Self, MeshError> {
        let n = shape[0].checked_mul(shape[1]).and_then(|v| v.checked_mul(shape[2]));
        if n != Some(data.len()) {
            return Err(MeshError::invalid("field samples do not match the grid shape"));
        }
        Ok(Self { shape, data })
    }

    #[must_use]
    #[inline]
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.shape[1] + j) * self.shape[2] + k
    }

    #[must_use]
    #[inline]
    pub fn at(&self, i: usize, j: usize, k: usize) -> f64 {
        self.data[self.index(i, j, k)]
    }

    #[must_use]
    pub fn min_max(&self) -> (f64, f64) {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for &v in self.data {
            if v.is_nan() {
                return (f64::NAN, f64::NAN);
            }
            lo = lo.min(v);
            hi = hi.max(v);
        }
        (lo, hi)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grid3 {
    pub shape: [usize; 3],
    pub data: Vec<f64>,
}

impl Grid3 {
    #[must_use]
    pub fn filled(shape: [usize; 3], value: f64) -> Self {
        Self { shape, data: vec![value; shape[0] * shape[1] * shape[2]] }
    }



    pub fn from_vec(shape: [usize; 3], data: Vec<f64>) -> Result<Self, MeshError> {
        Field3::new(shape, &data)?;
        Ok(Self { shape, data })
    }

    #[must_use]
    pub fn view(&self) -> Field3<'_> {
        Field3 { shape: self.shape, data: &self.data }
    }

    #[must_use]
    #[inline]
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.shape[1] + j) * self.shape[2] + k
    }

    #[must_use]
    #[inline]
    pub fn at(&self, i: usize, j: usize, k: usize) -> f64 {
        self.data[self.index(i, j, k)]
    }

    #[inline]
    pub fn at_mut(&mut self, i: usize, j: usize, k: usize) -> &mut f64 {
        let idx = self.index(i, j, k);
        &mut self.data[idx]
    }
}
