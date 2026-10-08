// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::soft::design::Interpolation;
use implexity_physics_solid::soft::model::{Formulation, NodalPotential, SoftModel};
use implexity_physics_solid::soft_fsi::voxel::{KuhnMesh, VoxelGrid};
use rayon::prelude::*;

use crate::problem::design::voxel_centroids;
use crate::problem::removal::{ModifierSpec, RemovalSpec};

const ACTIVE: f64 = 0.0;

#[derive(Debug)]
pub struct RemovalZoneModifier {
    model: SoftModel,
    sign: f64,
    saturation: f64,
    owner: Vec<usize>,
    representative: Vec<usize>,
    reference: Vec<f64>,
    kernel: Vec<Vec<(usize, f64)>>,
    tissue: Vec<usize>,
    nodes: usize,
}

impl RemovalZoneModifier {

    pub fn new(
        kuhn: &KuhnMesh,
        grid: &VoxelGrid,
        removal: &RemovalSpec,
        spec: &ModifierSpec,
        interpolation: Interpolation,
    ) -> CaeResult<Self> {
        let mesh = kuhn.mesh.clone();
        let ne = mesh.elements.len();
        let nodes = mesh.node_count();
        let fibres = vec![spec.fibre_directions.clone(); ne];
        let axis = vec![[0.0, 0.0, 1.0]; ne];
        let model = SoftModel::new(
            mesh,
            vec![spec.material.clone()],
            vec![0; ne],
            Formulation::Mixed { stabilization: 1.0 },
            fibres,
            axis,
            interpolation,
            vec![false; 3 * nodes],
            Vec::new(),
            false,
        )?;
        let n = grid.voxel_count();
        let mut representative = vec![usize::MAX; n];
        for (e, &v) in kuhn.owner.iter().enumerate() {
            if representative[v] == usize::MAX {
                representative[v] = e;
            }
        }
        let c = voxel_centroids(grid);
        let r = spec.band_m;
        let weight = |v: usize, w: usize| -> f64 {
            let d2 = (0..3).map(|a| (c[v][a] - c[w][a]).powi(2)).sum::<f64>();
            let q = d2 / (r * r);
            if q < 1.0 { (1.0 - q) * (1.0 - q) } else { 0.0 }
        };
        let h = grid.element_size_m;
        let reach: [isize; 3] = std::array::from_fn(|_| {
            #[allow(clippy::cast_possible_truncation)]
            let k = (r / h).ceil() as isize;
            k
        });
        let s = grid.shape;
        let index = |i: isize, j: isize, k: isize| -> Option<usize> {
            let (i, j, k) = (usize::try_from(i).ok()?, usize::try_from(j).ok()?, usize::try_from(k).ok()?);
            (i < s[0] && j < s[1] && k < s[2]).then_some((i * s[1] + j) * s[2] + k)
        };
        let kernel: Vec<Vec<(usize, f64)>> = (0..n)
            .map(|v| {
                if removal.reference[v] <= 0.0 {
                    return Vec::new();
                }
                #[allow(clippy::cast_possible_wrap)]
                let (vi, vj, vk) =
                    ((v / (s[1] * s[2])) as isize, ((v / s[2]) % s[1]) as isize, (v % s[2]) as isize);
                let mut total = 0.0;
                let mut row = Vec::new();
                for di in -reach[0]..=reach[0] {
                    for dj in -reach[1]..=reach[1] {
                        for dk in -reach[2]..=reach[2] {
                            let Some(w) = index(vi + di, vj + dj, vk + dk) else { continue };
                            let k = weight(v, w);
                            if k == 0.0 {
                                continue;
                            }
                            total += k * removal.reference[w];
                            if removal.removable[w] {
                                row.push((w, k));
                            }
                        }
                    }
                }
                if total > 0.0 {
                    for (_, k) in &mut row {
                        *k /= total;
                    }
                }
                row
            })
            .collect();
        let tissue: Vec<usize> = (0..ne).filter(|&e| removal.reference[kuhn.owner[e]] > 0.0).collect();
        Ok(Self {
            model,
            sign: spec.sign,
            saturation: spec.saturation,
            owner: kuhn.owner.clone(),
            representative,
            reference: removal.reference.clone(),
            kernel,
            tissue,
            nodes,
        })
    }

    fn check(&self, u: &[f64], params: &[f64]) -> CaeResult<()> {
        if u.len() != 3 * self.nodes || params.len() != 2 * self.owner.len() {
            return Err(CaeError::contract(
                "removal-zone modifier: displacement or parameter length mismatch",
            ));
        }
        Ok(())
    }

    fn indicator(&self, params: &[f64]) -> Vec<(f64, f64, f64)> {
        self.kernel
            .iter()
            .map(|row| {
                let s: f64 =
                    row.iter().map(|(w, k)| k * (self.reference[*w] - params[self.representative[*w]])).sum();
                let e = (-s / self.saturation).exp();
                (s, 1.0 - e, e / self.saturation)
            })
            .collect()
    }

    #[must_use]
    pub fn intensity(&self, params: &[f64]) -> Vec<f64> {
        self.indicator(params).into_iter().map(|(_, s, _)| s).collect()
    }

    fn local(&self, e: usize, u: &[f64]) -> [f64; 16] {
        let t = self.model.mesh.elements[e];
        let mut d = [0.0; 16];
        for a in 0..4 {
            for i in 0..3 {
                d[3 * a + i] = u[3 * t[a] + i];
            }
        }
        d
    }

    fn dofs(&self, e: usize) -> [usize; 12] {
        let t = self.model.mesh.elements[e];
        std::array::from_fn(|k| 3 * t[k / 3] + k % 3)
    }
}

impl NodalPotential for RemovalZoneModifier {
    fn energy(&self, u: &[f64], params: &[f64]) -> Result<f64, CaeError> {
        self.check(u, params)?;
        let ind = self.indicator(params);
        let parts: Vec<f64> = self
            .tissue
            .par_iter()
            .map(|&e| {
                let sigma = ind[self.owner[e]].1;
                if sigma <= ACTIVE {
                    return Ok(0.0);
                }
                let loc =
                    self.model.element_local(e, &self.local(e, u), params[e], 0.0, &[0.0; 6], 1.0, false)?;
                Ok(self.sign * sigma * loc.energy)
            })
            .collect::<CaeResult<Vec<f64>>>()?;
        Ok(parts.iter().sum())
    }

    fn force(&self, u: &[f64], params: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.check(u, params)?;
        let ind = self.indicator(params);
        let parts: Vec<Option<(usize, [f64; 16])>> = self
            .tissue
            .par_iter()
            .map(|&e| {
                let sigma = ind[self.owner[e]].1;
                if sigma <= ACTIVE {
                    return Ok(None);
                }
                let loc =
                    self.model.element_local(e, &self.local(e, u), params[e], 0.0, &[0.0; 6], 1.0, false)?;
                Ok(Some((e, loc.gradient.map(|g| self.sign * sigma * g))))
            })
            .collect::<CaeResult<_>>()?;
        let mut f = vec![0.0; u.len()];
        for (e, g) in parts.into_iter().flatten() {
            for (k, dof) in self.dofs(e).into_iter().enumerate() {
                f[dof] += g[k];
            }
        }
        Ok(f)
    }

    fn tangent(&self, u: &[f64], params: &[f64]) -> Result<Vec<(usize, usize, f64)>, CaeError> {
        self.check(u, params)?;
        let ind = self.indicator(params);
        let parts: Vec<Vec<(usize, usize, f64)>> = self
            .tissue
            .par_iter()
            .map(|&e| {
                let sigma = ind[self.owner[e]].1;
                if sigma <= ACTIVE {
                    return Ok(Vec::new());
                }
                let loc =
                    self.model.element_local(e, &self.local(e, u), params[e], 0.0, &[0.0; 6], 1.0, true)?;
                let dofs = self.dofs(e);
                let c = self.sign * sigma;
                let mut out = Vec::with_capacity(144);
                for a in 0..12 {
                    for b in 0..12 {
                        let v = c * loc.hessian[a * 16 + b];
                        if v != 0.0 {
                            out.push((dofs[a], dofs[b], v));
                        }
                    }
                }
                Ok(out)
            })
            .collect::<CaeResult<_>>()?;
        Ok(parts.into_iter().flatten().collect())
    }

    fn force_params_vjp(&self, u: &[f64], params: &[f64], w: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.check(u, params)?;
        if w.len() != u.len() {
            return Err(CaeError::contract("removal-zone modifier: cotangent length mismatch"));
        }
        let ind = self.indicator(params);
        let parts: Vec<(usize, f64, f64)> = self
            .tissue
            .par_iter()
            .map(|&e| {
                let full =
                    self.model.element_local_full(e, &self.local(e, u), params[e], 0.0, &[0.0; 6], 1.0)?;
                let dofs = self.dofs(e);
                let (mut t, mut m) = (0.0, 0.0);
                for (k, dof) in dofs.into_iter().enumerate() {
                    t += w[dof] * full.gradient[k];
                    m += w[dof] * full.design[k][0];
                }
                Ok((e, t, m))
            })
            .collect::<CaeResult<_>>()?;
        let mut g = vec![0.0; params.len()];
        let mut a = vec![0.0; self.kernel.len()];
        for (e, t, m) in parts {
            let v = self.owner[e];
            g[e] += self.sign * ind[v].1 * m;
            a[v] += self.sign * t * ind[v].2;
        }
        for (v, row) in self.kernel.iter().enumerate() {
            if a[v] == 0.0 {
                continue;
            }
            for (w, k) in row {
                g[self.representative[*w]] -= a[v] * k;
            }
        }
        Ok(g)
    }

    fn force_params_jacobian(&self, u: &[f64], params: &[f64]) -> Result<Vec<(usize, usize, f64)>, CaeError> {
        self.check(u, params)?;
        let ind = self.indicator(params);
        let parts: Vec<Vec<(usize, usize, f64)>> = self
            .tissue
            .par_iter()
            .map(|&e| {
                let full =
                    self.model.element_local_full(e, &self.local(e, u), params[e], 0.0, &[0.0; 6], 1.0)?;
                let v = self.owner[e];
                let (_, sigma, dsigma) = ind[v];
                let dofs = self.dofs(e);
                let row = &self.kernel[v];
                let mut out = Vec::with_capacity(12 * (row.len() + 1));
                for (k, dof) in dofs.into_iter().enumerate() {
                    let local = self.sign * sigma * full.design[k][0];
                    if local != 0.0 {
                        out.push((dof, e, local));
                    }
                    let c = -self.sign * dsigma * full.gradient[k];
                    if c != 0.0 {
                        for (w, kw) in row {
                            out.push((dof, self.representative[*w], c * kw));
                        }
                    }
                }
                Ok(out)
            })
            .collect::<CaeResult<_>>()?;
        Ok(parts.into_iter().flatten().collect())
    }
}
