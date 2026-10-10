// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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

