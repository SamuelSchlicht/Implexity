// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use implexity_core::{CaeError, CaeResult};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PointCloud {
    pub positions: Vec<[f64; 3]>,
    pub weights: Vec<f64>,
}

pub trait LagrangianCarrier: Send + Sync {
    fn trace_size(&self) -> usize;
    fn design_size(&self) -> usize;
    fn point_count(&self) -> usize;

    fn points(&self, trace: &[f64], design: &[f64]) -> CaeResult<PointCloud>;

    fn points_jvp(
        &self,
        trace: &[f64],
        design: &[f64],
        d_trace: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<PointCloud>;

    fn points_vjp(
        &self,
        trace: &[f64],
        design: &[f64],
        positions_bar: &[[f64; 3]],
        weights_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>)>;
    fn impulses_to_flux(&self, impulses: &[[f64; 3]], dt: f64) -> Vec<f64>;
    fn flux_to_impulses(&self, flux: &[f64], dt: f64) -> Vec<[f64; 3]>;
    fn velocities(&self, trace_start: &[f64], trace_end: &[f64], dt: f64) -> Vec<[f64; 3]> {
        let delta: Vec<f64> = trace_end.iter().zip(trace_start).map(|(e, s)| e - s).collect();
        self.flux_to_impulses(&delta, dt)
    }
    fn second_order(&self) -> Option<&dyn SecondOrderCarrier> {
        None
    }
}

pub trait SecondOrderCarrier: Send + Sync {

    fn points_vjp_tangent(
        &self,
        trace: &[f64],
        design: &[f64],
        positions_bar: &[[f64; 3]],
        weights_bar: &[f64],
        d_trace: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)>;
}

#[derive(Clone, Debug)]
pub struct RigidCarrier {
    reference: Vec<[f64; 3]>,
    weights: Vec<f64>,
    scaled: bool,
}

impl RigidCarrier {

    pub fn new(reference: Vec<[f64; 3]>, weights: Vec<f64>, scaled: bool) -> CaeResult<Self> {
        if reference.len() != weights.len()
            || reference.iter().flatten().any(|v| !v.is_finite())
            || weights.iter().any(|w| !(w.is_finite() && *w >= 0.0))
        {
            return Err(CaeError::contract(
                "rigid carrier requires finite positions and non-negative weights",
            ));
        }
        Ok(Self { reference, weights, scaled })
    }


    pub fn sampled(
        lo: [f64; 3],
        hi: [f64; 3],
        step: [f64; 3],
        inside: &dyn Fn([f64; 3]) -> bool,
    ) -> CaeResult<Self> {
        let mut reference = Vec::new();
        let mut weights = Vec::new();
        let count = |a: usize| -> usize {
            let n = ((hi[a] - lo[a]) / step[a]).round();
            if n.is_finite() && n >= 1.0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    n as usize
                }
            } else {
                0
            }
        };
        let (nx, ny, nz) = (count(0), count(1), count(2));
        let volume = step[0] * step[1] * step[2];
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nz {
                    let x = [
                        lo[0] + (i as f64 + 0.5) * step[0],
                        lo[1] + (j as f64 + 0.5) * step[1],
                        lo[2] + (k as f64 + 0.5) * step[2],
                    ];
                    if inside(x) {
                        reference.push(x);
                        weights.push(volume);
                    }
                }
            }
        }
        Self::new(reference, weights, false)
    }


    pub fn scaled_weights(mut self, factor: f64) -> CaeResult<Self> {
        if !(factor.is_finite() && factor >= 0.0) {
            return Err(CaeError::contract("rigid carrier weight factor must be finite and non-negative"));
        }
        for w in &mut self.weights {
            *w *= factor;
        }
        Ok(self)
    }

    fn scale(&self, design: &[f64]) -> f64 {
        if self.scaled { design.first().copied().unwrap_or(1.0) } else { 1.0 }
    }

    fn check(&self, trace: &[f64], design: &[f64]) -> CaeResult<()> {
        if trace.len() != 3 || design.len() != self.design_size() {
            return Err(CaeError::contract("rigid carrier: trace of length 3 and matching design required"));
        }
        Ok(())
    }
}

impl LagrangianCarrier for RigidCarrier {
    fn trace_size(&self) -> usize {
        3
    }

    fn design_size(&self) -> usize {
        usize::from(self.scaled)
    }

    fn point_count(&self) -> usize {
        self.reference.len()
    }

    fn points(&self, trace: &[f64], design: &[f64]) -> CaeResult<PointCloud> {
        self.check(trace, design)?;
        let s = self.scale(design);
        Ok(PointCloud {
            positions: self
                .reference
                .iter()
                .map(|x| [x[0] + trace[0], x[1] + trace[1], x[2] + trace[2]])
                .collect(),
            weights: self.weights.iter().map(|w| w * s).collect(),
        })
    }

    fn points_jvp(
        &self,
        trace: &[f64],
        design: &[f64],
        d_trace: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<PointCloud> {
        self.check(trace, design)?;
        let ds = if self.scaled { d_design.and_then(|d| d.first().copied()).unwrap_or(0.0) } else { 0.0 };
        Ok(PointCloud {
            positions: vec![[d_trace[0], d_trace[1], d_trace[2]]; self.reference.len()],
            weights: self.weights.iter().map(|w| w * ds).collect(),
        })
    }

    fn points_vjp(
        &self,
        trace: &[f64],
        design: &[f64],
        positions_bar: &[[f64; 3]],
        weights_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.check(trace, design)?;
        let mut t = [0.0; 3];
        for p in positions_bar {
            for d in 0..3 {
                t[d] += p[d];
            }
        }
        let design_bar = if self.scaled {
            vec![weights_bar.iter().zip(&self.weights).map(|(b, w)| b * w).sum()]
        } else {
            Vec::new()
        };
        Ok((t.to_vec(), design_bar))
    }

    fn impulses_to_flux(&self, impulses: &[[f64; 3]], dt: f64) -> Vec<f64> {
        let mut f = [0.0; 3];
        for i in impulses {
            for d in 0..3 {
                f[d] += i[d];
            }
        }
        f.iter().map(|v| v / dt).collect()
    }

    fn flux_to_impulses(&self, flux: &[f64], dt: f64) -> Vec<[f64; 3]> {
        vec![[flux[0] / dt, flux[1] / dt, flux[2] / dt]; self.reference.len()]
    }

    fn second_order(&self) -> Option<&dyn SecondOrderCarrier> {
        Some(self)
    }
}

impl SecondOrderCarrier for RigidCarrier {
    fn points_vjp_tangent(
        &self,
        trace: &[f64],
        design: &[f64],
        _positions_bar: &[[f64; 3]],
        _weights_bar: &[f64],
        _d_trace: &[f64],
        _d_design: Option<&[f64]>,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {

        self.check(trace, design)?;
        Ok((vec![0.0; 3], vec![0.0; self.design_size()]))
    }
}
