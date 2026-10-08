// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::CsrMatrix;

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub count: usize,
    pub components: usize,
    pub data: Vec<f64>,
}

impl Values {


    pub fn new(count: usize, components: usize, data: Vec<f64>) -> CaeResult<Self> {
        if data.len() != count * components {
            return Err(err("values have inconsistent dimensions"));
        }
        Ok(Self { count, components, data })
    }

    #[must_use]
    pub fn scalar(data: Vec<f64>) -> Self {
        Self { count: data.len(), components: 1, data }
    }
}

fn values<'a>(v: &'a Values, count: usize, name: &str) -> CaeResult<&'a Values> {
    if v.count != count || !v.data.iter().all(|x| x.is_finite()) {
        return Err(err(format!("{name}: expected finite values with leading size {count}")));
    }
    Ok(v)
}

fn apply(m: &CsrMatrix, v: &Values) -> Values {
    let k = v.components;
    let mut out = vec![0.0; m.nrows() * k];
    for i in 0..m.nrows() {
        let (idx, data) = m.row(i);
        for (j, a) in idx.iter().zip(data) {
            for c in 0..k {
                out[i * k + c] += a * v.data[j * k + c];
            }
        }
    }
    Values { count: m.nrows(), components: k, data: out }
}

fn owned(m: &CsrMatrix, name: &str) -> CaeResult<CsrMatrix> {
    if !m.data().iter().all(|v| v.is_finite()) {
        return Err(err(format!("{name}: nonfinite sparse values")));
    }
    Ok(m.clone())
}

fn row_sums(m: &CsrMatrix) -> Vec<f64> {
    (0..m.nrows()).map(|i| m.row(i).1.iter().sum()).collect()
}



pub fn dense_trace(m: &DenseMatrix) -> CaeResult<CsrMatrix> {
    let (mut rows, mut cols) = (Vec::with_capacity(m.data.len()), Vec::with_capacity(m.data.len()));
    for i in 0..m.nrows {
        for j in 0..m.ncols {
            rows.push(i);
            cols.push(j);
        }
    }
    CsrMatrix::from_triplets(m.nrows, m.ncols, &rows, &cols, &m.data).map_err(|e| err(e.to_string()))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExchangeVjp {
    pub traction: Values,
    pub measure: Vec<f64>,
    pub left_to_interface: DenseMatrix,
    pub right_to_interface: DenseMatrix,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceProjection {
    left: CsrMatrix,
    right: CsrMatrix,
    measure: Vec<f64>,
}

impl InterfaceProjection {


    pub fn new(left: &CsrMatrix, right: &CsrMatrix, measure: &[f64]) -> CaeResult<Self> {
        let l = owned(left, "left trace")?;
        let r = owned(right, "right trace")?;
        if !measure.iter().all(|v| v.is_finite()) {
            return Err(err("interface measure: invalid dimensions or nonfinite values"));
        }
        if l.nrows() != r.nrows()
            || measure.len() != l.nrows()
            || [l.nrows(), l.ncols(), r.nrows(), r.ncols()].contains(&0)
        {
            return Err(err("projection dimensions invalid"));
        }
        if measure.iter().any(|m| *m <= 0.0) {
            return Err(err("interface measures must be positive"));
        }
        for (name, p) in [("left", &l), ("right", &r)] {
            if row_sums(p).iter().any(|s| (s - 1.0).abs() > 1e-12) {
                return Err(err(format!("{name} trace must preserve constants (row sums equal one)")));
            }
        }
        Ok(Self { left: l, right: r, measure: measure.to_vec() })
    }

    #[must_use]
    pub fn left_to_interface(&self) -> &CsrMatrix {
        &self.left
    }

    #[must_use]
    pub fn right_to_interface(&self) -> &CsrMatrix {
        &self.right
    }

    #[must_use]
    pub fn interface_measure(&self) -> &[f64] {
        &self.measure
    }



    pub fn jump(&self, left: &Values, right: &Values) -> CaeResult<Values> {
        let l = values(left, self.left.ncols(), "left values")?;
        let r = values(right, self.right.ncols(), "right values")?;
        if l.components != r.components {
            return Err(err("left/right component shapes must agree"));
        }
        let (a, b) = (apply(&self.left, l), apply(&self.right, r));
        Ok(Values {
            count: a.count,
            components: a.components,
            data: a.data.iter().zip(&b.data).map(|(x, y)| x - y).collect(),
        })
    }

    fn weighted(&self, t: &Values) -> Values {
        let k = t.components;
        Values {
            count: t.count,
            components: k,
            data: t.data.iter().enumerate().map(|(i, v)| v * self.measure[i / k]).collect(),
        }
    }



    pub fn conservative_exchange(&self, traction: &Values) -> CaeResult<(Values, Values)> {
        let t = values(traction, self.measure.len(), "interface flux/traction")?;
        let q = self.weighted(t);
        let left = apply(&self.left.transpose(), &q);
        let mut right = apply(&self.right.transpose(), &q);
        for v in &mut right.data {
            *v = -*v;
        }
        Ok((left, right))
    }



    pub fn virtual_work_error(
        &self,
        left_virtual: &Values,
        right_virtual: &Values,
        traction: &Values,
    ) -> CaeResult<f64> {
        let jump = self.jump(left_virtual, right_virtual)?;
        let t = values(traction, self.measure.len(), "interface flux/traction")?;
        if t.components != jump.components {
            return Err(err("traction and virtual-value component shapes must agree"));
        }
        let (left, right) = self.conservative_exchange(t)?;
        let k = t.components;
        let lhs: f64 = (0..t.data.len()).map(|i| self.measure[i / k] * t.data[i] * jump.data[i]).sum();
        let rhs: f64 = left_virtual.data.iter().zip(&left.data).map(|(a, b)| a * b).sum::<f64>()
            + right_virtual.data.iter().zip(&right.data).map(|(a, b)| a * b).sum::<f64>();
        Ok(lhs - rhs)
    }



    pub fn exchange_jvp(
        &self,
        traction: &Values,
        dtraction: &Values,
        dleft: Option<&CsrMatrix>,
        dright: Option<&CsrMatrix>,
        dmeasure: Option<&[f64]>,
    ) -> CaeResult<(Values, Values)> {
        let n = self.measure.len();
        let t = values(traction, n, "traction")?;
        let dt = values(dtraction, n, "traction derivative")?;
        if dt.components != t.components {
            return Err(err("traction derivative has wrong shape"));
        }
        let empty = |m: &CsrMatrix| {
            CsrMatrix::from_triplets(m.nrows(), m.ncols(), &[], &[], &[]).map_err(|e| err(e.to_string()))
        };
        let dl = match dleft {
            Some(m) => owned(m, "left trace derivative")?,
            None => empty(&self.left)?,
        };
        let dr = match dright {
            Some(m) => owned(m, "right trace derivative")?,
            None => empty(&self.right)?,
        };
        for (name, dp, p) in [("left", &dl, &self.left), ("right", &dr, &self.right)] {
            if dp.shape() != p.shape() {
                return Err(err(format!("invalid {name} trace derivative")));
            }
            if row_sums(dp).iter().any(|s| s.abs() > 1e-12) {
                return Err(err(format!("{name} trace derivative must preserve constants")));
            }
        }
        let dm = dmeasure.map_or_else(|| vec![0.0; n], <[f64]>::to_vec);
        if dm.len() != n || !dm.iter().all(|v| v.is_finite()) {
            return Err(err("invalid measure derivative"));
        }
        let k = t.components;
        let q = self.weighted(t);
        let dq = Values {
            count: n,
            components: k,
            data: (0..n * k).map(|i| self.measure[i / k] * dt.data[i] + dm[i / k] * t.data[i]).collect(),
        };
        let sum = |a: Values, b: Values, sign: f64| Values {
            count: a.count,
            components: a.components,
            data: a.data.iter().zip(&b.data).map(|(x, y)| sign * (x + y)).collect(),
        };
        let left = sum(apply(&dl.transpose(), &q), apply(&self.left.transpose(), &dq), 1.0);
        let right = sum(apply(&dr.transpose(), &q), apply(&self.right.transpose(), &dq), -1.0);
        Ok((left, right))
    }



    pub fn exchange_vjp(
        &self,
        traction: &Values,
        left_cotangent: &Values,
        right_cotangent: &Values,
    ) -> CaeResult<ExchangeVjp> {
        let n = self.measure.len();
        let t = values(traction, n, "traction")?;
        let a = values(left_cotangent, self.left.ncols(), "left cotangent")?;
        let b = values(right_cotangent, self.right.ncols(), "right cotangent")?;
        if a.components != t.components || b.components != t.components {
            return Err(err("cotangent component shapes must match traction"));
        }
        let k = t.components;
        let (la, rb) = (apply(&self.left, a), apply(&self.right, b));
        let qbar: Vec<f64> = la.data.iter().zip(&rb.data).map(|(x, y)| x - y).collect();
        let q = self.weighted(t);
        let outer = |cot: &Values, sign: f64| -> DenseMatrix {
            let mut m = DenseMatrix::zeros(n, cot.count);
            for i in 0..n {
                for j in 0..cot.count {
                    let s: f64 = (0..k).map(|c| q.data[i * k + c] * cot.data[j * k + c]).sum();
                    m.data[i * cot.count + j] = sign * s;
                }
            }
            m
        };
        Ok(ExchangeVjp {
            traction: Values {
                count: n,
                components: k,
                data: (0..n * k).map(|i| self.measure[i / k] * qbar[i]).collect(),
            },
            measure: (0..n).map(|i| (0..k).map(|c| qbar[i * k + c] * t.data[i * k + c]).sum()).collect(),
            left_to_interface: outer(a, 1.0),
            right_to_interface: outer(b, -1.0),
        })
    }
}



pub fn normalized_overlap_projection(overlap: &DenseMatrix, side_measure: &[f64]) -> CaeResult<DenseMatrix> {
    if overlap.nrows == 0 || overlap.ncols == 0 || side_measure.len() != overlap.ncols {
        return Err(err("overlap/measure mismatch"));
    }
    if !overlap.data.iter().chain(side_measure).all(|v| v.is_finite())
        || overlap.data.iter().any(|v| *v < 0.0)
        || side_measure.iter().any(|m| *m <= 0.0)
    {
        return Err(err("overlap must be finite nonnegative; side measures finite positive"));
    }
    let cols = overlap.ncols;
    let mut out = overlap.clone();
    for row in out.data.chunks_mut(cols) {
        let s: f64 = row.iter().sum();
        if s <= 0.0 {
            return Err(err("every interface quadrature segment must overlap the side"));
        }
        for v in row {
            *v /= s;
        }
    }
    Ok(out)
}

