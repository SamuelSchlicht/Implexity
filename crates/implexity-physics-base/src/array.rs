// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_ad::Scalar;

use crate::model_errors::{PhysicsError, PhysicsResult};

#[derive(Debug, Clone, PartialEq)]
pub struct Tensor<S> {
    shape: Vec<usize>,
    data: Vec<S>,
}

fn py_shape(shape: &[usize]) -> String {
    match shape.len() {
        1 => format!("({},)", shape[0]),
        _ => format!("({})", shape.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
    }
}


pub fn broadcast_shapes(a: &[usize], b: &[usize]) -> PhysicsResult<Vec<usize>> {
    let n = a.len().max(b.len());
    let mut out = vec![0; n];
    for i in 0..n {
        let da = if i + a.len() >= n { a[i + a.len() - n] } else { 1 };
        let db = if i + b.len() >= n { b[i + b.len() - n] } else { 1 };
        out[i] = if da == db || db == 1 {
            da
        } else if da == 1 {
            db
        } else {
            return Err(PhysicsError::value(format!(
                "operands could not be broadcast together with shapes {} {}",
                py_shape(a),
                py_shape(b)
            )));
        };
    }
    Ok(out)
}

fn strides(shape: &[usize]) -> Vec<usize> {
    let mut s = vec![1; shape.len()];
    for i in (0..shape.len().saturating_sub(1)).rev() {
        s[i] = s[i + 1] * shape[i + 1];
    }
    s
}

fn broadcast_index(shape: &[usize], target: &[usize]) -> Vec<usize> {
    let total: usize = target.iter().product();
    let offset = target.len() - shape.len();
    let src_strides = strides(shape);
    let mut out = Vec::with_capacity(total);
    let mut index = vec![0usize; target.len()];
    for _ in 0..total {
        let mut flat = 0;
        for (k, &dim) in shape.iter().enumerate() {
            if dim != 1 {
                flat += index[k + offset] * src_strides[k];
            }
        }
        out.push(flat);
        for axis in (0..target.len()).rev() {
            index[axis] += 1;
            if index[axis] < target[axis] {
                break;
            }
            index[axis] = 0;
        }
    }
    out
}

impl<S: Scalar> Tensor<S> {
    pub fn scalar(value: S) -> Self {
        Self { shape: Vec::new(), data: vec![value] }
    }


    pub fn from_vec(shape: Vec<usize>, data: Vec<S>) -> PhysicsResult<Self> {
        if shape.iter().product::<usize>() != data.len() {
            return Err(PhysicsError::value(format!(
                "cannot reshape array of size {} into shape {}",
                data.len(),
                py_shape(&shape)
            )));
        }
        Ok(Self { shape, data })
    }

    #[must_use]
    pub fn vector(data: Vec<S>) -> Self {
        Self { shape: vec![data.len()], data }
    }

    pub fn full(shape: &[usize], value: S) -> Self {
        Self { shape: shape.to_vec(), data: vec![value; shape.iter().product()] }
    }

    #[must_use]
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }

    #[must_use]
    pub fn ndim(&self) -> usize {
        self.shape.len()
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.data.len()
    }

    #[must_use]
    pub fn data(&self) -> &[S] {
        &self.data
    }

    #[must_use]
    pub fn into_data(self) -> Vec<S> {
        self.data
    }

    #[must_use]
    pub fn at(&self, i: usize) -> S {
        self.data[i]
    }


    pub fn item(&self) -> PhysicsResult<S> {
        if self.data.len() == 1 {
            Ok(self.data[0])
        } else {
            Err(PhysicsError::value("only size-1 arrays can be converted to Python scalars"))
        }
    }

    #[must_use]
    pub fn values(&self) -> Tensor<f64> {
        Tensor { shape: self.shape.clone(), data: self.data.iter().map(Scalar::value).collect() }
    }

    #[must_use]
    pub fn map(&self, f: impl Fn(S) -> S) -> Self {
        Self { shape: self.shape.clone(), data: self.data.iter().map(|&x| f(x)).collect() }
    }


    pub fn zip(&self, other: &Self, f: impl Fn(S, S) -> S) -> PhysicsResult<Self> {
        if self.shape == other.shape {
            return Ok(Self {
                shape: self.shape.clone(),
                data: self.data.iter().zip(&other.data).map(|(&a, &b)| f(a, b)).collect(),
            });
        }
        let shape = broadcast_shapes(&self.shape, &other.shape)?;
        let ia = broadcast_index(&self.shape, &shape);
        let ib = broadcast_index(&other.shape, &shape);
        let data = ia.iter().zip(&ib).map(|(&i, &j)| f(self.data[i], other.data[j])).collect();
        Ok(Self { shape, data })
    }


    pub fn broadcast_to(&self, shape: &[usize]) -> PhysicsResult<Self> {
        let joined = broadcast_shapes(&self.shape, shape)?;
        if joined != shape {
            return Err(PhysicsError::value(format!(
                "operands could not be broadcast together with remapped shapes [original->remapped]: {}  and requested shape {}",
                py_shape(&self.shape),
                py_shape(shape)
            )));
        }
        let idx = broadcast_index(&self.shape, shape);
        Ok(Self { shape: shape.to_vec(), data: idx.into_iter().map(|i| self.data[i]).collect() })
    }


    pub fn add(&self, o: &Self) -> PhysicsResult<Self> {
        self.zip(o, |a, b| a + b)
    }


    pub fn sub(&self, o: &Self) -> PhysicsResult<Self> {
        self.zip(o, |a, b| a - b)
    }


    pub fn mul(&self, o: &Self) -> PhysicsResult<Self> {
        self.zip(o, |a, b| a * b)
    }


    pub fn div(&self, o: &Self) -> PhysicsResult<Self> {
        self.zip(o, |a, b| a / b)
    }

    #[must_use]
    pub fn scale(&self, c: f64) -> Self {
        self.map(|x| x * c)
    }

    #[must_use]
    pub fn expand_last(&self) -> Self {
        let mut shape = self.shape.clone();
        shape.push(1);
        Self { shape, data: self.data.clone() }
    }

    #[must_use]
    pub fn expand_last2(&self) -> Self {
        self.expand_last().expand_last()
    }


    pub fn reshape(&self, shape: &[usize]) -> PhysicsResult<Self> {
        Self::from_vec(shape.to_vec(), self.data.clone())
    }

    #[must_use]
    pub fn sum(&self) -> S {
        pairwise_sum(&self.data)
    }

    #[must_use]
    pub fn mean(&self) -> S {
        self.sum() / self.data.len() as f64
    }

    fn choose(&self, pick: impl Fn(f64, f64) -> bool) -> PhysicsResult<S> {
        let Some(first) = self.data.first() else {
            return Err(PhysicsError::value(
                "zero-size array to reduction operation minimum which has no identity",
            ));
        };
        if self.data.iter().any(|x| x.value().is_nan()) {
            return Ok(S::from_f64(f64::NAN));
        }
        let mut best = first.value();
        for x in &self.data {
            if pick(x.value(), best) {
                best = x.value();
            }
        }
        #[allow(clippy::float_cmp)]                                
        let ties: Vec<S> = self.data.iter().copied().filter(|x| x.value() == best).collect();
        if ties.len() == 1 {
            return Ok(ties[0]);
        }
        let n = ties.len();
        let grad = vec![1.0 / n as f64; n];
        let hess = vec![0.0; n * n];
        Ok(S::lift(best, &ties, &grad, &hess))
    }


    pub fn min(&self) -> PhysicsResult<S> {
        self.choose(|x, best| x < best)
    }


    pub fn max(&self) -> PhysicsResult<S> {
        self.choose(|x, best| x > best)
    }


    pub fn reduce_last(&self, f: impl Fn(&[S]) -> S) -> PhysicsResult<Self> {
        let Some((&last, lead)) = self.shape.split_last() else {
            return Err(PhysicsError::value("axis -1 is out of bounds for array of dimension 0"));
        };
        let data = if last == 0 {
            vec![f(&[]); lead.iter().product()]
        } else {
            self.data.chunks(last).map(f).collect()
        };
        Ok(Self { shape: lead.to_vec(), data })
    }


    pub fn sum_last(&self) -> PhysicsResult<Self> {
        self.reduce_last(pairwise_sum)
    }


    pub fn map_blocks(
        &self,
        k: usize,
        out_tail: &[usize],
        f: impl Fn(&[S]) -> PhysicsResult<Vec<S>>,
    ) -> PhysicsResult<Self> {
        if self.shape.len() < k {
            return Err(PhysicsError::value("array has too few dimensions"));
        }
        let lead = &self.shape[..self.shape.len() - k];
        let block: usize = self.shape[self.shape.len() - k..].iter().product();
        let out_block: usize = out_tail.iter().product();
        let mut data = Vec::with_capacity(lead.iter().product::<usize>() * out_block);
        let count: usize = lead.iter().product();
        for i in 0..count {
            let out = f(&self.data[i * block..(i + 1) * block])?;
            if out.len() != out_block {
                return Err(PhysicsError::value("block function returned a wrong size"));
            }
            data.extend(out);
        }
        let mut shape = lead.to_vec();
        shape.extend_from_slice(out_tail);
        Ok(Self { shape, data })
    }


    pub fn stack(items: &[Self]) -> PhysicsResult<Self> {
        let Some(first) = items.first() else {
            return Err(PhysicsError::value("need at least one array to stack"));
        };
        if items.iter().any(|t| t.shape != first.shape) {
            return Err(PhysicsError::value("all input arrays must have the same shape"));
        }
        let mut shape = vec![items.len()];
        shape.extend_from_slice(&first.shape);
        Ok(Self { shape, data: items.iter().flat_map(|t| t.data.iter().copied()).collect() })
    }
}

pub fn pairwise_sum<S: Scalar>(xs: &[S]) -> S {
    const BLOCK: usize = 8;
    const PW: usize = 128;
    let n = xs.len();
    if n < BLOCK {
        let mut res = S::zero();
        for &x in xs {
            res += x;
        }
        return res;
    }
    if n <= PW {
        let mut r = [S::zero(); BLOCK];
        r.copy_from_slice(&xs[..BLOCK]);
        let mut i = BLOCK;
        while i + BLOCK <= n {
            for k in 0..BLOCK {
                r[k] += xs[i + k];
            }
            i += BLOCK;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        for &x in &xs[i..] {
            res += x;
        }
        return res;
    }
    let mut n2 = n / 2;
    n2 -= n2 % BLOCK;
    pairwise_sum(&xs[..n2]) + pairwise_sum(&xs[n2..])
}

impl Tensor<f64> {

    pub fn from_json(value: &Value) -> PhysicsResult<Self> {
        fn walk(v: &Value, depth: usize, shape: &mut Vec<usize>, data: &mut Vec<f64>) -> PhysicsResult<()> {
            match v {
                Value::Array(items) => {
                    if shape.len() == depth {
                        shape.push(items.len());
                    } else if shape.len() < depth || shape[depth] != items.len() {
                        return Err(PhysicsError::value(
                            "setting an array element with a sequence. The requested array has an inhomogeneous shape",
                        ));
                    }
                    for item in items {
                        walk(item, depth + 1, shape, data)?;
                    }
                    Ok(())
                }
                Value::Number(n) => {
                    if shape.len() != depth {
                        return Err(PhysicsError::value(
                            "setting an array element with a sequence. The requested array has an inhomogeneous shape",
                        ));
                    }
                    data.push(n.as_f64().unwrap_or(f64::NAN));
                    Ok(())
                }
                Value::Bool(b) => {
                    if shape.len() != depth {
                        return Err(PhysicsError::value(
                            "setting an array element with a sequence. The requested array has an inhomogeneous shape",
                        ));
                    }
                    data.push(if *b { 1.0 } else { 0.0 });
                    Ok(())
                }
                other => Err(PhysicsError::value(format!(
                    "could not convert {} to float",
                    implexity_core::pyobj::repr(other)
                ))),
            }
        }
        let mut shape = Vec::new();
        let mut data = Vec::new();
        walk(value, 0, &mut shape, &mut data)?;
        Self::from_vec(shape, data)
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        fn build(shape: &[usize], data: &[f64]) -> Value {
            match shape.split_first() {
                None => json!(data[0]),
                Some((&n, rest)) => {
                    let block: usize = rest.iter().product();
                    Value::Array((0..n).map(|i| build(rest, &data[i * block..(i + 1) * block])).collect())
                }
            }
        }
        build(&self.shape, &self.data)
    }

    #[must_use]
    pub fn lift<S: Scalar>(&self) -> Tensor<S> {
        Tensor { shape: self.shape.clone(), data: self.data.iter().map(|&v| S::from_f64(v)).collect() }
    }
}

