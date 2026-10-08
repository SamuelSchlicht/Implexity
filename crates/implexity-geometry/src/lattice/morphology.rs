// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#[must_use]
pub fn ball_offsets(spacing: [f64; 3], radius: f64) -> Vec<[i64; 3]> {
    let h_max = spacing.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let reach = radius + 0.5 * h_max;
    let bound = reach * reach * (1.0 + 1e-12);
    #[allow(clippy::cast_possible_truncation)]
    let counts: [i64; 3] = std::array::from_fn(|a| (reach / spacing[a] + 1e-12).floor() as i64);
    let mut out = Vec::new();
    for i in -counts[0]..=counts[0] {
        for j in -counts[1]..=counts[1] {
            for k in -counts[2]..=counts[2] {
                #[allow(clippy::cast_precision_loss)]
                let [x, y, z] = [i as f64 * spacing[0], j as f64 * spacing[1], k as f64 * spacing[2]];
                if x * x + y * y + z * z <= bound {
                    out.push([i, j, k]);
                }
            }
        }
    }
    out
}

fn shifted(shape: [usize; 3], cell: [usize; 3], offset: [i64; 3]) -> usize {
    let at = |a: usize| {
        #[allow(clippy::cast_possible_wrap)]
        let v = cell[a] as i64 + offset[a];
        #[allow(clippy::cast_possible_wrap)]
        let top = shape[a] as i64 - 1;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let c = v.clamp(0, top) as usize;
        c
    };
    (at(0) * shape[1] + at(1)) * shape[2] + at(2)
}

fn extremum_sources(values: &[f64], shape: [usize; 3], offsets: &[[i64; 3]], minimum: bool) -> Vec<usize> {
    let mut out = Vec::with_capacity(values.len());
    for i in 0..shape[0] {
        for j in 0..shape[1] {
            for k in 0..shape[2] {
                let cell = [i, j, k];
                let mut source = shifted(shape, cell, offsets[0]);
                let mut best = values[source];
                for o in &offsets[1..] {
                    let index = shifted(shape, cell, *o);
                    let candidate = values[index];
                    let better = if minimum { candidate < best } else { candidate > best };
                    if better {
                        best = candidate;
                        source = index;
                    }
                }
                out.push(source);
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opening {
    pub source: Vec<usize>,
}

impl Opening {

    #[must_use]
    pub fn new(values: &[f64], shape: [usize; 3], spacing: [f64; 3], radius: f64) -> Self {
        let offsets = ball_offsets(spacing, radius);
        let eroded_source = extremum_sources(values, shape, &offsets, true);
        let eroded: Vec<f64> = eroded_source.iter().map(|s| values[*s]).collect();
        let dilated_source = extremum_sources(&eroded, shape, &offsets, false);
        Self { source: dilated_source.iter().map(|s| eroded_source[*s]).collect() }
    }

    #[must_use]
    pub fn apply(&self, values: &[f64]) -> Vec<f64> {
        self.source.iter().map(|s| values[*s]).collect()
    }

    #[must_use]
    pub fn transpose(&self, adjoint: &[f64]) -> Vec<f64> {
        let mut out = vec![0.0; adjoint.len()];
        for (s, a) in self.source.iter().zip(adjoint) {
            out[*s] += a;
        }
        out
    }
}

