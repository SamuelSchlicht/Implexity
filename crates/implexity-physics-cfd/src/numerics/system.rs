// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_linalg::{CsrMatrix, LinalgError, LinearOperator};
use serde_json::{Map, Value};

use crate::error::{CfdError, CfdResult};


pub fn checked_vector(value: Option<&[f64]>, size: usize, name: &str) -> CfdResult<Vec<f64>> {
    let Some(v) = value else { return Ok(vec![0.0; size]) };
    if v.len() != size {
        return Err(CfdError::Contract(format!("{name} has size {}; expected {size}.", v.len())));
    }
    if !v.iter().all(|x| x.is_finite()) {
        return Err(CfdError::Contract(format!("{name} contains non-finite values.")));
    }
    Ok(v.to_vec())
}



#[derive(Debug, Clone)]
pub struct SaddlePointSystem {
    pub a: CsrMatrix,
    pub b: CsrMatrix,
    pub c: CsrMatrix,
    pub velocity_rhs: Vec<f64>,
    pub continuity_rhs: Vec<f64>,
    pub metadata: Map<String, Value>,
    bt: CsrMatrix,
    at: CsrMatrix,
    ct: CsrMatrix,
}

impl SaddlePointSystem {

    pub fn new(
        a: CsrMatrix,
        b: CsrMatrix,
        c: Option<CsrMatrix>,
        velocity_rhs: Option<&[f64]>,
        continuity_rhs: Option<&[f64]>,
        metadata: Map<String, Value>,
    ) -> CfdResult<Self> {
        if a.nrows() != a.ncols() {
            return Err(CfdError::Contract("A must be square.".into()));
        }
        if b.ncols() != a.nrows() {
            return Err(CfdError::Contract("B must have one column per velocity degree of freedom.".into()));
        }
        let n_p = b.nrows();
        let c = match c {
            None => CsrMatrix::try_new(n_p, n_p, vec![0; n_p + 1], Vec::new(), Vec::new())?,
            Some(c) => c,
        };
        if c.shape() != (n_p, n_p) {
            return Err(CfdError::Contract(
                "C must be square with one row per pressure degree of freedom.".into(),
            ));
        }
        if !a.is_finite() || !b.is_finite() || !c.is_finite() {
            return Err(CfdError::Contract("The block matrices contain non-finite values.".into()));
        }
        let n_u = a.nrows();
        let velocity_rhs = checked_vector(velocity_rhs, n_u, "velocity_rhs")?;
        let continuity_rhs = checked_vector(continuity_rhs, n_p, "continuity_rhs")?;
        let bt = b.transpose();
        let at = a.transpose();
        let ct = c.transpose();
        Ok(Self { a, b, c, velocity_rhs, continuity_rhs, metadata, bt, at, ct })
    }

    #[must_use]
    pub fn n_velocity(&self) -> usize {
        self.a.nrows()
    }

    #[must_use]
    pub fn n_pressure(&self) -> usize {
        self.b.nrows()
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.n_velocity() + self.n_pressure()
    }

    #[must_use]
    pub fn b_transpose(&self) -> &CsrMatrix {
        &self.bt
    }

    #[must_use]
    pub fn rhs(&self) -> Vec<f64> {
        let mut out = self.velocity_rhs.clone();
        out.extend_from_slice(&self.continuity_rhs);
        out
    }


    pub fn join(&self, velocity: &[f64], pressure: &[f64]) -> CfdResult<Vec<f64>> {
        let mut u = checked_vector(Some(velocity), self.n_velocity(), "velocity")?;
        let p = checked_vector(Some(pressure), self.n_pressure(), "pressure")?;
        u.extend_from_slice(&p);
        Ok(u)
    }


    pub fn split(&self, state: &[f64]) -> CfdResult<(Vec<f64>, Vec<f64>)> {
        let w = checked_vector(Some(state), self.size(), "state")?;
        let (u, p) = w.split_at(self.n_velocity());
        Ok((u.to_vec(), p.to_vec()))
    }


    pub fn sparse_matrix(&self, transpose: bool) -> CfdResult<CsrMatrix> {
        let n_u = self.n_velocity();
        let n = self.size();
        let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
        let mut push = |m: &CsrMatrix, r0: usize, c0: usize, scale: f64| {
            for i in 0..m.nrows() {
                let (ci, vi) = m.row(i);
                for (j, v) in ci.iter().zip(vi) {
                    rows.push(r0 + i);
                    cols.push(c0 + j);
                    vals.push(scale * v);
                }
            }
        };
        push(&self.a, 0, 0, 1.0);
        push(&self.bt, 0, n_u, 1.0);
        push(&self.b, n_u, 0, 1.0);
        push(&self.c, n_u, n_u, -1.0);
        let m = CsrMatrix::from_triplets(n, n, &rows, &cols, &vals)?;
        Ok(if transpose { m.transpose() } else { m })
    }


    pub fn apply(&self, x: &[f64], y: &mut [f64], transpose: bool) -> Result<(), LinalgError> {
        let n_u = self.n_velocity();
        let n = self.size();
        if x.len() != n || y.len() != n {
            return Err(LinalgError::Shape(format!(
                "saddle operator of order {n} applied to {} → {}",
                x.len(),
                y.len()
            )));
        }
        let (u, p) = x.split_at(n_u);
        let (a, c) = if transpose { (&self.at, &self.ct) } else { (&self.a, &self.c) };
        let au = a.matvec(u)?;
        let btp = self.bt.matvec(p)?;
        let bu = self.b.matvec(u)?;
        let cp = c.matvec(p)?;
        for i in 0..n_u {
            y[i] = au[i] + btp[i];
        }
        for i in 0..self.n_pressure() {
            y[n_u + i] = bu[i] - cp[i];
        }
        Ok(())
    }

    #[must_use]
    pub fn operator(&self, transpose: bool) -> SaddleOperator<'_> {
        SaddleOperator { system: self, transpose }
    }


    pub fn block_residuals(
        &self,
        state: &[f64],
        rhs: Option<&[f64]>,
        transpose: bool,
    ) -> CfdResult<(Vec<f64>, Vec<f64>)> {
        let w = checked_vector(Some(state), self.size(), "state")?;
        let b = match rhs {
            None => self.rhs(),
            Some(r) => checked_vector(Some(r), self.size(), "rhs")?,
        };
        let mut r = vec![0.0; self.size()];
        self.apply(&w, &mut r, transpose)?;
        for (ri, bi) in r.iter_mut().zip(&b) {
            *ri -= bi;
        }
        let (ru, rp) = r.split_at(self.n_velocity());
        Ok((ru.to_vec(), rp.to_vec()))
    }
}

#[derive(Clone, Copy)]
pub struct SaddleOperator<'a> {
    system: &'a SaddlePointSystem,
    transpose: bool,
}

impl LinearOperator for SaddleOperator<'_> {
    fn n(&self) -> usize {
        self.system.size()
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        self.system.apply(x, y, self.transpose)
    }
}
