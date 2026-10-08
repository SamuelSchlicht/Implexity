// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use core::cell::RefCell;
use core::fmt;
use core::iter::Sum;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

const CONSTANT: u32 = u32::MAX;

#[derive(Default)]
struct Tape {
    starts: Vec<u32>,
    parents: Vec<u32>,
    partials: Vec<f64>,
    active: bool,
}

impl Tape {
    fn push(&mut self, parents: &[(u32, f64)]) -> u32 {
        if self.starts.is_empty() {
            self.starts.push(0);
        }
        for &(p, d) in parents {
            if p != CONSTANT && d != 0.0 {
                self.parents.push(p);
                self.partials.push(d);
            }
        }
        let end = u32::try_from(self.parents.len()).unwrap_or(u32::MAX);
        self.starts.push(end);
        u32::try_from(self.starts.len() - 2).unwrap_or(CONSTANT)
    }

    fn nodes(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }
}

thread_local! {
    static TAPE: RefCell<Tape> = RefCell::new(Tape::default());
}

#[derive(Clone, Copy, PartialEq)]
pub struct Rv {
    v: f64,
    id: u32,
}

impl Default for Rv {
    fn default() -> Self {
        Self { v: 0.0, id: CONSTANT }
    }
}

impl fmt::Debug for Rv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rv({}, #{})", self.v, self.id)
    }
}

fn record(v: f64, parents: &[(u32, f64)]) -> Rv {
    if parents.iter().all(|(p, _)| *p == CONSTANT) {
        return Rv { v, id: CONSTANT };
    }
    let id = TAPE.with(|t| t.borrow_mut().push(parents));
    Rv { v, id }
}

pub struct Recording {
    _private: (),
}

impl Recording {

    pub fn start() -> CaeResult<Self> {
        TAPE.with(|t| {
            let mut t = t.borrow_mut();
            if t.active {
                return Err(CaeError::contract("nested reverse-mode recordings are not supported"));
            }
            t.starts.clear();
            t.parents.clear();
            t.partials.clear();
            t.active = true;
            Ok(())
        })?;
        Ok(Self { _private: () })
    }

    #[must_use]
    pub fn inputs(&self, values: &[f64]) -> Vec<Rv> {
        values
            .iter()
            .map(|v| {
                let id = TAPE.with(|t| t.borrow_mut().push(&[]));
                Rv { v: *v, id }
            })
            .collect()
    }

    #[must_use]
    pub fn vjp(&self, outputs: &[Rv], cotangent: &[f64], inputs: &[Rv]) -> Vec<f64> {
        TAPE.with(|t| {
            let t = t.borrow();
            let mut adj = vec![0.0; t.nodes()];
            for (o, c) in outputs.iter().zip(cotangent) {
                if o.id != CONSTANT && *c != 0.0 {
                    adj[o.id as usize] += c;
                }
            }
            for k in (0..t.nodes()).rev() {
                let a = adj[k];
                if a == 0.0 {
                    continue;
                }
                for s in t.starts[k] as usize..t.starts[k + 1] as usize {
                    adj[t.parents[s] as usize] += a * t.partials[s];
                }
            }
            inputs.iter().map(|x| if x.id == CONSTANT { 0.0 } else { adj[x.id as usize] }).collect()
        })
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        TAPE.with(|t| {
            let mut t = t.borrow_mut();
            t.active = false;
            t.starts = Vec::new();
            t.parents = Vec::new();
            t.partials = Vec::new();
        });
    }
}

impl Add for Rv {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        record(self.v + o.v, &[(self.id, 1.0), (o.id, 1.0)])
    }
}

impl Sub for Rv {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        record(self.v - o.v, &[(self.id, 1.0), (o.id, -1.0)])
    }
}

impl Mul for Rv {
    type Output = Self;
    fn mul(self, o: Self) -> Self {
        record(self.v * o.v, &[(self.id, o.v), (o.id, self.v)])
    }
}

impl Div for Rv {
    type Output = Self;
    fn div(self, o: Self) -> Self {
        let q = self.v / o.v;
        record(q, &[(self.id, 1.0 / o.v), (o.id, -q / o.v)])
    }
}

impl Neg for Rv {
    type Output = Self;
    fn neg(self) -> Self {
        record(-self.v, &[(self.id, -1.0)])
    }
}

impl Add<f64> for Rv {
    type Output = Self;
    fn add(self, c: f64) -> Self {
        record(self.v + c, &[(self.id, 1.0)])
    }
}

impl Sub<f64> for Rv {
    type Output = Self;
    fn sub(self, c: f64) -> Self {
        record(self.v - c, &[(self.id, 1.0)])
    }
}

impl Mul<f64> for Rv {
    type Output = Self;
    fn mul(self, c: f64) -> Self {
        record(self.v * c, &[(self.id, c)])
    }
}

impl Div<f64> for Rv {
    type Output = Self;
    fn div(self, c: f64) -> Self {
        record(self.v / c, &[(self.id, 1.0 / c)])
    }
}

impl AddAssign for Rv {
    fn add_assign(&mut self, o: Self) {
        *self = *self + o;
    }
}

impl SubAssign for Rv {
    fn sub_assign(&mut self, o: Self) {
        *self = *self - o;
    }
}

impl MulAssign for Rv {
    fn mul_assign(&mut self, o: Self) {
        *self = *self * o;
    }
}

impl DivAssign for Rv {
    fn div_assign(&mut self, o: Self) {
        *self = *self / o;
    }
}

impl Sum for Rv {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::from_f64(0.0), |a, b| a + b)
    }
}

impl Scalar for Rv {
    fn from_f64(value: f64) -> Self {
        Self { v: value, id: CONSTANT }
    }

    fn value(&self) -> f64 {
        self.v
    }

    fn chain(self, f: f64, df: f64, _d2f: f64) -> Self {
        record(f, &[(self.id, df)])
    }

    fn chain2(a: Self, b: Self, f: f64, fa: f64, fb: f64, _faa: f64, _fab: f64, _fbb: f64) -> Self {
        record(f, &[(a.id, fa), (b.id, fb)])
    }

    fn lift(f: f64, inputs: &[Self], grad: &[f64], _hess: &[f64]) -> Self {
        let parents: Vec<(u32, f64)> = inputs.iter().zip(grad).map(|(x, g)| (x.id, *g)).collect();
        record(f, &parents)
    }
}

