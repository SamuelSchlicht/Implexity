// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use super::ad::interp;

#[derive(Clone, Debug, PartialEq)]
pub struct MaterialPhase {
    pub name: String,
    pub t: Vec<f64>,
    pub e: Vec<f64>,
    pub sy: Vec<f64>,
    pub k: Vec<f64>,
    pub nu: f64,
    pub alpha: f64,
    pub density: f64,
    pub cp: f64,
    pub t_limit: f64,
    pub t_melt: f64,
    pub source: String,
}

impl MaterialPhase {
    pub fn validate(&self) -> Result<(), String> {
        let n = self.t.len();
        if n == 0 || self.t.iter().any(|v| !v.is_finite() || *v <= 0.0)
            || self.t.windows(2).any(|v| v[0] >= v[1])
        {
            return Err(format!("phase '{}': temperatures must be positive, finite and strictly increasing", self.name));
        }
        for (a, v) in [("E", &self.e), ("sy", &self.sy), ("k", &self.k)] {
            if v.len() != n {
                return Err(format!(
                    "phase '{}': table {a} has {} entries against {n} temperatures",
                    self.name,
                    v.len()
                ));
            }
            if v.iter().any(|value| !value.is_finite() || *value < 0.0 || (a != "sy" && *value == 0.0)) {
                return Err(format!("phase '{}': table {a} contains invalid values", self.name));
            }
        }
        if !self.nu.is_finite() || self.nu <= -1.0 || self.nu >= 0.5
            || !self.alpha.is_finite()
            || [self.density, self.cp, self.t_limit, self.t_melt].iter().any(|v| !v.is_finite() || *v <= 0.0)
        {
            return Err(format!("phase '{}': material constants are invalid", self.name));
        }
        Ok(())
    }

    pub fn try_interp<S: Scalar>(&self, which: &str, t: S) -> Result<S, String> {
        self.validate()?;
        if !t.value().is_finite() {
            return Err("interpolation temperature must be finite".into());
        }
        let fp = match which {
            "E" => &self.e,
            "sy" => &self.sy,
            "k" => &self.k,
            _ => return Err(format!("unknown material table {which:?}")),
        };
        Ok(interp(t, &self.t, fp))
    }

    pub fn interp<S: Scalar>(&self, which: &str, t: S) -> S {
        self.try_interp(which, t).expect("invalid material interpolation")
    }

    #[must_use]
    pub fn t_range(&self) -> (f64, f64) {
        (self.t.first().copied().unwrap_or(f64::NAN), self.t.last().copied().unwrap_or(f64::NAN))
    }
}

pub struct PhaseSet<'a> {
    pub phases: &'a [MaterialPhase],
}

impl PhaseSet<'_> {
    #[must_use]
    pub fn n(&self) -> usize {
        self.phases.len()
    }

    pub fn validate_weights<S: Scalar>(&self, w: &[S]) -> Result<(), String> {
        if self.phases.is_empty() || !(w.len() == self.n() || (self.n() == 2 && w.len() == 1)) {
            return Err(format!("{} phases require matching weights or one binary fraction", self.n()));
        }
        if w.iter().any(|v| !v.value().is_finite() || v.value() < 0.0)
            || (self.n() == 2 && w.len() == 1 && w[0].value() > 1.0)
            || (w.len() == self.n() && w.iter().map(|v| v.value()).sum::<f64>() <= 1e-300)
        {
            return Err("phase weights must be finite, nonnegative and have positive total weight".into());
        }
        Ok(())
    }

    pub fn try_weights<S: Scalar>(&self, w: &[S]) -> Result<Vec<S>, String> {
        self.validate_weights(w)?;
        Ok(self.normalized_weights(w))
    }

    pub fn weights<S: Scalar>(&self, w: &[S]) -> Vec<S> {
        self.try_weights(w).expect("invalid phase weights")
    }

    fn normalized_weights<S: Scalar>(&self, w: &[S]) -> Vec<S> {
        if self.n() == 2 && w.len() == 1 {
            return vec![-w[0] + 1.0, w[0]];
        }
        let s = w.iter().fold(S::zero(), |a, b| a + *b);
        let d = if s.value().abs() > 1e-300 { s } else { S::one() };
        w.iter().map(|x| *x / d).collect()
    }

    pub fn from_logits<S: Scalar>(logits: &[S], sharpness: f64) -> Vec<S> {
        let z: Vec<S> = logits.iter().map(|x| *x * sharpness).collect();
        let m = z.iter().fold(S::from_f64(f64::NEG_INFINITY), |a, b| a.maximum(*b));
        let e: Vec<S> = z.iter().map(|x| (*x - m).exp()).collect();
        let s = e.iter().fold(S::zero(), |a, b| a + *b);
        e.iter().map(|x| *x / s).collect()
    }

    fn mix_table<S: Scalar>(&self, which: &str, t: S, w: &[S]) -> S {
        let w = self.weights(w);
        let mut out = S::zero();
        for (i, p) in self.phases.iter().enumerate() {
            out += w[i] * p.interp(which, t);
        }
        out
    }

    fn mix_const<S: Scalar>(&self, f: impl Fn(&MaterialPhase) -> f64, w: &[S]) -> S {
        let w = self.weights(w);
        let mut out = S::zero();
        for (i, p) in self.phases.iter().enumerate() {
            out += w[i] * f(p);
        }
        out
    }

    pub fn conductivity<S: Scalar>(&self, t: S, w: &[S]) -> S {
        self.mix_table("k", t, w)
    }
    pub fn modulus<S: Scalar>(&self, t: S, w: &[S]) -> S {
        self.mix_table("E", t, w)
    }
    pub fn yield_strength<S: Scalar>(&self, t: S, w: &[S]) -> S {
        self.mix_table("sy", t, w)
    }
    pub fn expansion<S: Scalar>(&self, w: &[S]) -> S {
        self.mix_const(|p| p.alpha, w)
    }
    pub fn rho_cp<S: Scalar>(&self, w: &[S]) -> S {
        self.mix_const(|p| p.density * p.cp, w)
    }
    pub fn surface_limit<S: Scalar>(&self, w: &[S]) -> S {
        self.mix_const(|p| p.t_limit, w)
    }
    pub fn melt_limit<S: Scalar>(&self, w: &[S]) -> S {
        self.mix_const(|p| p.t_melt, w)
    }
    pub fn poisson<S: Scalar>(&self, w: &[S]) -> S {
        self.mix_const(|p| p.nu, w)
    }

}
