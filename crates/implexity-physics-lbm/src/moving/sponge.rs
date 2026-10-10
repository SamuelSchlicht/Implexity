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




use implexity_core::{CaeError, CaeResult};

use super::boundary::{Face, Signal};
use crate::d3q19::Grid;

#[derive(Clone, Debug, PartialEq)]
pub struct SpongeSpec {
    pub face: Face,
    pub thickness_cells: usize,
    pub strength: f64,
    pub pressure_pa: f64,
    pub velocity_m_s: [f64; 3],
    pub wave: Option<TravellingWave>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TravellingWave {
    pub amplitude_m_s: [f64; 3],
    pub signal: Signal,
    pub direction: [f64; 3],
    pub speed_m_s: f64,
    pub origin_m: [f64; 3],
}

impl TravellingWave {

    pub fn validate(&self) -> CaeResult<()> {
        let finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
        let norm = self.direction.iter().map(|v| v * v).sum::<f64>().sqrt();
        if !(finite(&self.amplitude_m_s)
            && finite(&self.direction)
            && finite(&self.origin_m)
            && self.speed_m_s.is_finite()
            && self.speed_m_s > 0.0
            && self.signal.frequency_hz.is_finite()
            && self.signal.frequency_hz > 0.0
            && self.signal.phase_rad.is_finite()
            && self.signal.ramp_s.is_finite()
            && self.signal.ramp_s >= 0.0
            && (norm - 1.0).abs() <= 1e-9)
        {
            return Err(CaeError::contract(
                "sponge wave needs finite amplitude and origin, a unit direction, a positive speed and a positive frequency",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn phase(&self, x_m: [f64; 3]) -> f64 {
        let s: f64 = (0..3).map(|a| (x_m[a] - self.origin_m[a]) * self.direction[a]).sum();
        2.0 * std::f64::consts::PI * self.signal.frequency_hz * s / self.speed_m_s
    }
}

#[must_use]
pub fn ramp(s: f64) -> f64 {
    if s <= 0.0 {
        0.0
    } else if s >= 1.0 {
        1.0
    } else {
        s * s * s * (10.0 - 15.0 * s + 6.0 * s * s)
    }
}

#[derive(Clone, Debug)]
pub struct SpongeField {
    pub sigma: Vec<f64>,
    pub parts: Vec<Vec<(usize, f64)>>,
    pub slot: Vec<u32>,
    pub phases: Vec<Vec<[f64; 2]>>,
}

impl SpongeField {

    pub fn new(
        grid: Grid,
        periodic: [bool; 3],
        specs: &[SpongeSpec],
        origin_m: [f64; 3],
        spacing_m: f64,
    ) -> CaeResult<Self> {
        let n = grid.cells();
        let mut sigma = vec![0.0; n];
        let mut parts: Vec<Vec<(usize, f64)>> = Vec::new();
        let mut slot = vec![u32::MAX; n];
        let mut phases: Vec<Vec<[f64; 2]>> = Vec::new();
        for (l, s) in specs.iter().enumerate() {
            if let Some(w) = &s.wave {
                w.validate()?;
            }
            let axis = s.face.axis();
            if periodic[axis] {
                return Err(CaeError::contract(format!(
                    "sponge on face {} lies on a periodic axis",
                    s.face.name()
                )));
            }
            if s.thickness_cells == 0 || 2 * s.thickness_cells > grid.shape[axis] {
                return Err(CaeError::contract(format!(
                    "sponge on face {} must be between 1 cell and half the lattice thick",
                    s.face.name()
                )));
            }
            if !(s.strength.is_finite() && s.strength > 0.0 && s.strength < 1.0) {
                return Err(CaeError::contract("sponge strength must lie in (0, 1)"));
            }
            if !s.pressure_pa.is_finite() || s.velocity_m_s.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract("sponge far-field state must be finite"));
            }
            let len = s.thickness_cells as f64;
            for x in 0..n {
                let c = grid.coords(x)[axis];
                let depth = if s.face.inward() > 0 {
                    s.thickness_cells as f64 - c as f64
                } else {
                    c as f64 - (grid.shape[axis] - s.thickness_cells) as f64 + 1.0
                };
                if depth <= 0.0 {
                    continue;
                }
                let sv = s.strength * ramp((depth - 0.5) / len);
                if sv <= 0.0 {
                    continue;
                }
                sigma[x] += sv;
                if slot[x] == u32::MAX {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        slot[x] = parts.len() as u32;
                    }
                    parts.push(Vec::new());
                    phases.push(Vec::new());
                }
                parts[slot[x] as usize].push((l, sv));
                let phase = s.wave.as_ref().map_or([1.0, 0.0], |w| {
                    let ijk = grid.coords(x);
                    #[allow(clippy::cast_precision_loss)]
                    let theta =
                        w.phase(std::array::from_fn(|a| origin_m[a] + (ijk[a] as f64 + 0.5) * spacing_m));
                    [theta.cos(), theta.sin()]
                });
                phases[slot[x] as usize].push(phase);
            }
        }
        if sigma.iter().any(|&s| s >= 1.0) {
            return Err(CaeError::contract("summed sponge strength must stay below one"));
        }
        Ok(Self { sigma, parts, slot, phases })
    }
}

