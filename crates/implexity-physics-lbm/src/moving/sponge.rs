// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

