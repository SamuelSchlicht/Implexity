// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use crate::error::{GResult, GeometryError};
use crate::lattice::numerics::trilerp_const;
use crate::node::Attr;
use crate::pyfmt::PyObj;
use crate::scalar::Scalar;

pub const FROZEN_SCHEMA: &str = "implexity-frozen-geometry/1";

#[must_use]
pub fn encode_runs(mask: &[bool]) -> Vec<[usize; 2]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < mask.len() {
        if mask[i] {
            let a = i;
            while i < mask.len() && mask[i] {
                i += 1;
            }
            out.push([a, i - a]);
        } else {
            i += 1;
        }
    }
    out
}

fn runs_err() -> GeometryError {
    GeometryError::Value("invalid or overlapping held runs".into())
}


pub fn decode_runs(runs: &Attr, count: usize) -> GResult<Vec<bool>> {
    let Attr::List(items) = runs else {
        return Err(GeometryError::Value("held runs must be a sequence".into()));
    };
    let mut out = vec![false; count];
    let mut last = 0usize;
    for item in items {
        let Attr::List(pair) = item else {
            return Err(GeometryError::Value("held run requires start and count".into()));
        };
        if pair.len() != 2 {
            return Err(GeometryError::Value("held run requires start and count".into()));
        }
        let (Attr::Int(a), Attr::Int(n)) = (&pair[0], &pair[1]) else {
            return Err(runs_err());
        };
        let (a, n) = (*a, *n);
        if a < 0 || n < 1 {
            return Err(runs_err());
        }
        let (a, n) =
            (usize::try_from(a).map_err(|_| runs_err())?, usize::try_from(n).map_err(|_| runs_err())?);
        if a < last || a + n > count {
            return Err(runs_err());
        }
        for v in &mut out[a..a + n] {
            *v = true;
        }
        last = a + n;
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrozenGeometry {
    pub shape: [usize; 3],
    pub origin_mm: [f64; 3],
    pub domain_mm: [f64; 3],
    pub occupancy_mask: Vec<bool>,
    pub phase_mask: Vec<bool>,
    pub occupancy: Vec<f64>,
    pub phase_fraction: Vec<f64>,
    occupancy_maskf: Vec<f64>,
    phase_maskf: Vec<f64>,
}

fn attr_f64(a: &Attr) -> Option<f64> {
    match a {
        Attr::Int(i) => {
            #[allow(clippy::cast_precision_loss)]
            let v = *i as f64;
            Some(v)
        }
        Attr::Float(f) => Some(*f),
        Attr::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

fn numeric_values(a: &Attr) -> Option<Vec<f64>> {
    fn rec(a: &Attr, out: &mut Vec<f64>, any_bool: &mut bool) -> bool {
        match a {
            Attr::List(v) => v.iter().all(|x| rec(x, out, any_bool)),
            Attr::Int(i) => {
                #[allow(clippy::cast_precision_loss)]
                out.push(*i as f64);
                true
            }
            Attr::Float(f) => {
                out.push(*f);
                true
            }
            Attr::Bool(b) => {
                *any_bool = true;
                out.push(f64::from(u8::from(*b)));
                true
            }
            Attr::Array(arr) => {
                if !matches!(arr.dtype().kind(), 'i' | 'u' | 'f') {
                    return false;
                }
                out.extend(arr.to_f64_vec());
                true
            }
            _ => false,
        }
    }
    let mut out = Vec::new();
    let mut any_bool = false;
    let ok = rec(a, &mut out, &mut any_bool);

    let all_bool = any_bool && !matches!(a, Attr::Array(_)) && {
        fn only_bools(a: &Attr) -> bool {
            match a {
                Attr::List(v) => v.iter().all(only_bools),
                Attr::Bool(_) => true,
                _ => false,
            }
        }
        only_bools(a)
    };
    (ok && !all_bool).then_some(out)
}

const FROZEN_KEYS: [&str; 8] = [
    "schema",
    "shape",
    "origin_mm",
    "domain_mm",
    "occupancy_runs",
    "phase_runs",
    "occupancy",
    "phase_fraction",
];

impl FrozenGeometry {

    pub fn normalise(raw: Option<&Attr>) -> GResult<Option<Self>> {
        let raw = match raw {
            None | Some(Attr::Null) => return Ok(None),
            Some(r) => r,
        };
        let bad = || GeometryError::Value("invalid frozen geometry declaration".into());
        let Attr::Dict(map) = raw else { return Err(bad()) };
        let get = |k: &str| map.iter().find(|(n, _)| n == k).map(|(_, v)| v);
        let keys: std::collections::BTreeSet<&str> = map.iter().map(|(k, _)| k.as_str()).collect();
        let want: std::collections::BTreeSet<&str> = FROZEN_KEYS.into_iter().collect();
        if get("schema").and_then(Attr::as_str) != Some(FROZEN_SCHEMA) || keys != want {
            return Err(bad());
        }
        let shape_err = || {
            GeometryError::Value(
                "frozen geometry shape must contain three integer counts >=2, at most two million cells"
                    .into(),
            )
        };
        let shape: Vec<usize> = match get("shape") {
            Some(Attr::List(v)) => v
                .iter()
                .map(|x| match x {
                    Attr::Int(i) if *i >= 2 => usize::try_from(*i).ok(),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(shape_err)?,
            _ => return Err(shape_err()),
        };
        if shape.len() != 3 || shape.iter().product::<usize>() > 2_000_000 {
            return Err(shape_err());
        }
        let shape = [shape[0], shape[1], shape[2]];
        let count = shape.iter().product::<usize>();
        let reg_err = || GeometryError::Value("invalid frozen geometry registration".into());
        let triple = |k: &str, positive: bool| -> GResult<[f64; 3]> {
            let v = get(k).and_then(numeric_values).ok_or_else(reg_err)?;
            if v.len() != 3
                || !matches!(get(k), Some(Attr::List(l)) if l.len() == 3 && l.iter().all(|x| attr_f64(x).is_some()))
            {
                return Err(reg_err());
            }
            if !v.iter().all(|x| x.is_finite()) || (positive && v.iter().any(|x| *x <= 0.0)) {
                return Err(reg_err());
            }
            Ok([v[0], v[1], v[2]])
        };
        let origin_mm = triple("origin_mm", false)?;
        let domain_mm = triple("domain_mm", true)?;
        let occupancy_mask = decode_runs(get("occupancy_runs").unwrap_or(&Attr::Null), count)?;
        let phase_mask = decode_runs(get("phase_runs").unwrap_or(&Attr::Null), count)?;
        let val_err = || {
            GeometryError::Value("frozen occupancy and neutral phase must be finite values in [0,1]".into())
        };
        let values = |k: &str| -> GResult<Vec<f64>> {
            let v = get(k).and_then(numeric_values).ok_or_else(val_err)?;
            if v.len() != count || !v.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)) {
                return Err(val_err());
            }
            Ok(v)
        };
        let occupancy = values("occupancy")?;
        let phase_fraction = values("phase_fraction")?;
        let tof = |m: &[bool]| m.iter().map(|b| if *b { 1.0 } else { 0.0 }).collect();
        let (occupancy_maskf, phase_maskf) = (tof(&occupancy_mask), tof(&phase_mask));
        Ok(Some(Self {
            shape,
            origin_mm,
            domain_mm,
            occupancy_mask,
            phase_mask,
            occupancy,
            phase_fraction,
            occupancy_maskf,
            phase_maskf,
        }))
    }

    fn runs_json(mask: &[bool]) -> Value {
        Value::Array(encode_runs(mask).into_iter().map(|[a, n]| json!([a, n])).collect())
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "schema": FROZEN_SCHEMA, "shape": self.shape, "origin_mm": self.origin_mm, "domain_mm": self.domain_mm,
            "occupancy_runs": Self::runs_json(&self.occupancy_mask), "phase_runs": Self::runs_json(&self.phase_mask),
            "occupancy": self.occupancy, "phase_fraction": self.phase_fraction,
        })
    }

    #[must_use]
    pub fn to_attr(&self) -> Attr {
        let floats = |v: &[f64]| Attr::List(v.iter().map(|x| Attr::Float(*x)).collect());
        #[allow(clippy::cast_possible_wrap)]
        let runs = |m: &[bool]| {
            Attr::List(
                encode_runs(m)
                    .into_iter()
                    .map(|[a, n]| Attr::List(vec![Attr::Int(a as i64), Attr::Int(n as i64)]))
                    .collect(),
            )
        };
        #[allow(clippy::cast_possible_wrap)]
        let shape = Attr::List(self.shape.iter().map(|n| Attr::Int(*n as i64)).collect());
        Attr::Dict(vec![
            ("schema".into(), Attr::Str(FROZEN_SCHEMA.into())),
            ("shape".into(), shape),
            ("origin_mm".into(), floats(&self.origin_mm)),
            ("domain_mm".into(), floats(&self.domain_mm)),
            ("occupancy_runs".into(), runs(&self.occupancy_mask)),
            ("phase_runs".into(), runs(&self.phase_mask)),
            ("occupancy".into(), floats(&self.occupancy)),
            ("phase_fraction".into(), floats(&self.phase_fraction)),
        ])
    }

    #[must_use]
    pub fn py_obj(&self) -> PyObj {
        self.to_attr().py_obj()
    }

    fn masks(&self) -> [(&[bool], &[f64]); 2] {
        [(&self.occupancy_mask, &self.occupancy), (&self.phase_mask, &self.phase_fraction)]
    }

    #[must_use]
    pub fn matches_grid(&self, shape: [usize; 3], origin_mm: [f64; 3], domain_mm: [f64; 3]) -> bool {
        self.shape == shape && self.origin_mm == origin_mm && self.domain_mm == domain_mm
    }

    pub fn apply_grid(&self, rho: &mut [f64], phase: &mut [f64]) {
        for (field, (mask, reference)) in [rho, phase].into_iter().zip(self.masks()) {
            for ((v, m), r) in field.iter_mut().zip(mask).zip(reference) {
                if *m {
                    *v = *r;
                }
            }
        }
    }

    pub fn weights_at<S: Scalar>(&self, p: [S; 3]) -> (S, S, S, S) {
        let h: [f64; 3] = std::array::from_fn(|a| {
            #[allow(clippy::cast_precision_loss)]
            let n = self.shape[a] as f64;
            self.domain_mm[a] / n
        });
        let ijk: [S; 3] = std::array::from_fn(|a| (p[a] - self.origin_mm[a]) / h[a] - 0.5);
        let inside = (0..3)
            .all(|a| p[a].val() >= self.origin_mm[a] && p[a].val() <= self.origin_mm[a] + self.domain_mm[a]);
        let mut out = [S::cst(0.0); 4];
        for (i, (mask, reference)) in self.masks().into_iter().enumerate() {
            if !mask.iter().any(|m| *m) {
                continue;
            }
            let mf: &[f64] = if i == 0 { &self.occupancy_maskf } else { &self.phase_maskf };
            let w = if inside { trilerp_const(mf, self.shape, ijk).clip_c(0.0, 1.0) } else { S::cst(0.0) };
            out[2 * i] = w;
            out[2 * i + 1] = trilerp_const(reference, self.shape, ijk);
        }
        (out[0], out[1], out[2], out[3])
    }

    #[must_use]
    pub fn active(&self) -> [bool; 2] {
        [self.occupancy_mask.iter().any(|m| *m), self.phase_mask.iter().any(|m| *m)]
    }

    pub fn apply_at<S: Scalar>(&self, rho: S, phase: S, p: [S; 3]) -> (S, S) {
        let [ar, ap] = self.active();
        let (wr, rr, wp, rp) = self.weights_at(p);
        let rho = if ar { (-wr + 1.0) * rho + wr * rr } else { rho };
        let phase = if ap { (-wp + 1.0) * phase + wp * rp } else { phase };
        (rho, phase)
    }
}
