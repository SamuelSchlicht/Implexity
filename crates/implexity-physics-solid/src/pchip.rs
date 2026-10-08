// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::CaeError;

#[derive(Debug, Clone, PartialEq)]
pub struct PropertyCurve {
    pub knots: Vec<f64>,
    pub coefficients: [Vec<f64>; 4],
    pub integral_coefficients: [Vec<f64>; 5],
}

fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

fn edge_case(h0: f64, h1: f64, m0: f64, m1: f64) -> f64 {
    let d = ((2.0 * h0 + h1) * m0 - h0 * m1) / (h0 + h1);
    #[allow(clippy::float_cmp)]                                 
    let mask = sign(d) != sign(m0);
    #[allow(clippy::float_cmp)]
    let mask2 = sign(m0) != sign(m1) && d.abs() > 3.0 * m0.abs();
    if mask {
        0.0
    } else if mask2 {
        3.0 * m0
    } else {
        d
    }
}

fn derivatives(x: &[f64], y: &[f64]) -> Vec<f64> {
    let n = x.len();
    let hk: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).collect();
    let mk: Vec<f64> = (0..n - 1).map(|i| (y[i + 1] - y[i]) / hk[i]).collect();
    if n == 2 {
        return vec![mk[0], mk[0]];
    }
    let mut dk = vec![0.0; n];
    for i in 0..n - 2 {
        #[allow(clippy::float_cmp)]                                 
        let condition = sign(mk[i + 1]) != sign(mk[i]) || mk[i + 1] == 0.0 || mk[i] == 0.0;
        if !condition {
            let w1 = 2.0 * hk[i + 1] + hk[i];
            let w2 = hk[i + 1] + 2.0 * hk[i];
            let whmean = (w1 / mk[i] + w2 / mk[i + 1]) / (w1 + w2);
            dk[i + 1] = 1.0 / whmean;
        }
    }
    dk[0] = edge_case(hk[0], hk[1], mk[0], mk[1]);
    dk[n - 1] = edge_case(hk[n - 2], hk[n - 3], mk[n - 2], mk[n - 3]);
    dk
}

fn is_real(v: &serde_json::Value) -> bool {
    v.is_number()
}

impl PropertyCurve {

    pub fn from_json(t: &serde_json::Value, values: &serde_json::Value) -> Result<Self, CaeError> {
        let parse = |v: &serde_json::Value| -> Option<Vec<f64>> {
            let a = v.as_array()?;
            if !a.iter().all(is_real) {
                return None;
            }
            a.iter().map(serde_json::Value::as_f64).collect()
        };
        let (Some(knots), Some(y)) = (parse(t), parse(values)) else {
            return Err(CaeError::contract(
                "property table requires finite real numbers, not strings or booleans",
            ));
        };
        Self::new(knots, &y)
    }


    pub fn new(knots: Vec<f64>, values: &[f64]) -> Result<Self, CaeError> {
        let n = knots.len();
        if n < 2
            || values.len() != n
            || !knots.iter().chain(values).all(Scalar::is_finite)
            || knots.windows(2).any(|w| w[1] - w[0] <= 0.0)
        {
            return Err(CaeError::contract(
                "finite property table on a strictly increasing temperature grid required",
            ));
        }
        let dydx = derivatives(&knots, values);
        let m = n - 1;
        let mut c: [Vec<f64>; 4] = std::array::from_fn(|_| vec![0.0; m]);
        for i in 0..m {
            let dx = knots[i + 1] - knots[i];
            let slope = (values[i + 1] - values[i]) / dx;
            let t = (dydx[i] + dydx[i + 1] - 2.0 * slope) / dx;
            c[0][i] = t / dx;
            c[1][i] = (slope - dydx[i]) / dx - t;
            c[2][i] = dydx[i];
            c[3][i] = values[i];
        }
        let mut ic: [Vec<f64>; 5] = std::array::from_fn(|_| vec![0.0; m]);
        for i in 0..m {
            ic[0][i] = c[0][i] / 4.0;
            ic[1][i] = c[1][i] / 3.0;
            ic[2][i] = c[2][i] / 2.0;
            ic[3][i] = c[3][i];
        }
        for ip in 1..m {
            let h = knots[ip] - knots[ip - 1];
            let mut r = 0.0;
            let mut z = 1.0;
            for k in (0..5).rev() {
                r += ic[k][ip - 1] * z;
                z *= h;
            }
            ic[4][ip] = r;
        }
        Ok(Self { knots, coefficients: c, integral_coefficients: ic })
    }

    fn interval(&self, t: f64) -> usize {
        let pos = self.knots.partition_point(|k| *k <= t);
        pos.saturating_sub(1).min(self.knots.len() - 2)
    }

    fn inside(&self, t: f64) -> bool {
        t >= self.knots[0] && t <= self.knots[self.knots.len() - 1]
    }

    fn horner<S: Scalar>(&self, t: S, coefficients: &[Vec<f64>]) -> S {
        let v = t.value();
        if !self.inside(v) {
            return S::from_f64(f64::NAN);
        }
        let i = self.interval(v);
        let q = t - self.knots[i];
        let mut out = S::zero();
        for a in coefficients {
            out = out * q + a[i];
        }
        out
    }

    pub fn value<S: Scalar>(&self, t: S) -> S {
        self.horner(t, &self.coefficients)
    }

    pub fn primitive<S: Scalar>(&self, t: S) -> S {
        self.horner(t, &self.integral_coefficients)
    }

    pub fn derivative<S: Scalar>(&self, t: S) -> S {
        let v = t.value();
        if !self.inside(v) {
            return S::from_f64(f64::NAN);
        }
        let i = self.interval(v);
        let q = t - self.knots[i];
        (q * (3.0 * self.coefficients[0][i]) + 2.0 * self.coefficients[1][i]) * q + self.coefficients[2][i]
    }

    #[must_use]
    pub fn first_knot(&self) -> f64 {
        self.knots[0]
    }

    #[must_use]
    pub fn last_knot(&self) -> f64 {
        self.knots[self.knots.len() - 1]
    }
}
