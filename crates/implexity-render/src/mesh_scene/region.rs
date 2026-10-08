// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use super::{SceneError, dot, length, scale, sub};

#[derive(Clone, Debug, PartialEq)]
pub struct Convex {
    planes: Vec<([f64; 3], [f64; 3])>,
}

impl Convex {
    #[must_use]
    pub fn aabb(lo: [f64; 3], hi: [f64; 3]) -> Self {
        let mut planes = Vec::with_capacity(6);
        for a in 0..3 {
            let mut n = [0.0; 3];
            n[a] = -1.0;
            planes.push((lo, n));
            let mut n = [0.0; 3];
            n[a] = 1.0;
            planes.push((hi, n));
        }
        Self { planes }
    }



    pub fn half_space(point: [f64; 3], normal: [f64; 3]) -> Result<Self, SceneError> {
        let l = length(normal);
        if l <= 1e-12 {
            return Err(SceneError::Invalid("a half-space needs a nonzero normal".into()));
        }
        Ok(Self { planes: vec![(point, scale(normal, 1.0 / l))] })
    }

    fn depth(&self, p: [f64; 3]) -> f64 {
        self.planes.iter().map(|(q, n)| dot(sub(p, *q), *n)).fold(f64::NEG_INFINITY, f64::max)
    }

    fn interval(&self, o: [f64; 3], d: [f64; 3]) -> Option<Span> {
        let mut span =
            Span { t_in: f64::NEG_INFINITY, n_in: [0.0; 3], t_out: f64::INFINITY, n_out: [0.0; 3] };
        for (q, n) in &self.planes {
            let dn = dot(d, *n);
            let s = dot(sub(o, *q), *n);
            if dn.abs() <= 1e-15 {
                if s > 0.0 {
                    return None;
                }
                continue;
            }
            let t = -s / dn;
            if dn < 0.0 {
                if t > span.t_in {
                    span.t_in = t;
                    span.n_in = *n;
                }
            } else if t < span.t_out {
                span.t_out = t;
                span.n_out = *n;
            }
        }
        (span.t_in < span.t_out).then_some(span)
    }
}

#[derive(Clone, Copy, Debug)]
struct Span {
    t_in: f64,
    n_in: [f64; 3],
    t_out: f64,
    n_out: [f64; 3],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Region {
    pub keep: Vec<Convex>,
    pub remove: Vec<Convex>,
    pub tolerance_mm: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CapEntry {
    pub t: f64,
    pub normal: [f64; 3],
}

impl Region {
    #[must_use]
    pub fn is_unbounded(&self) -> bool {
        self.keep.is_empty() && self.remove.is_empty()
    }

    #[must_use]
    pub fn contains(&self, p: [f64; 3]) -> bool {
        let tol = self.tolerance_mm;
        self.keep.iter().all(|c| c.depth(p) < -tol) && self.remove.iter().all(|c| c.depth(p) > tol)
    }

    #[must_use]
    pub fn cap_entry(&self, o: [f64; 3], d: [f64; 3], t_hit: f64) -> Option<CapEntry> {

        let mut candidates: Vec<(f64, [f64; 3])> = Vec::new();
        for c in &self.keep {
            if let Some(s) = c.interval(o, d)
                && s.t_in.is_finite()
                && s.t_in <= t_hit
            {
                candidates.push((s.t_in, s.n_in));
            }
        }
        for c in &self.remove {
            if let Some(s) = c.interval(o, d)
                && s.t_out.is_finite()
                && s.t_out <= t_hit
            {
                candidates.push((s.t_out, s.n_out.map(|x| -x)));
            }
        }
        candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
        let step = 4.0 * self.tolerance_mm.max(1e-9);
        let at = |t: f64| [o[0] + t * d[0], o[1] + t * d[1], o[2] + t * d[2]];

        for (t, n) in candidates {
            if !self.contains(at(t - step)) && self.contains(at((t + step).min(t_hit))) {
                return Some(CapEntry { t, normal: n });
            }
        }
        None
    }
}
