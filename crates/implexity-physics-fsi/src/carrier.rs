// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use implexity_ad::HyperDual;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::carrier::{LagrangianCarrier, PointCloud, SecondOrderCarrier};
use implexity_physics_solid::hyperelastic::kinematics::TetMesh;
use implexity_physics_solid::soft_fsi::pushforward::{BlockingMap, PushforwardPoints};
use implexity_physics_solid::soft_fsi::voxel::VoxelDesignMap;

#[derive(Debug, Clone)]
pub struct SolidCarrier {
    points: Arc<PushforwardPoints>,
    map: VoxelDesignMap,
    blocking: BlockingMap,
    scale: f64,
    constant: bool,
    geometry: Option<Arc<Geometry>>,
}

#[derive(Debug)]
struct Geometry {
    elements: Vec<[usize; 4]>,
    gradients: Vec<[[f64; 3]; 4]>,
    weights: Vec<f64>,
}

type Mat3 = [[f64; 3]; 3];

fn cofactor(f: &Mat3) -> Mat3 {
    [
        [
            f[1][1] * f[2][2] - f[1][2] * f[2][1],
            f[1][2] * f[2][0] - f[1][0] * f[2][2],
            f[1][0] * f[2][1] - f[1][1] * f[2][0],
        ],
        [
            f[0][2] * f[2][1] - f[0][1] * f[2][2],
            f[0][0] * f[2][2] - f[0][2] * f[2][0],
            f[0][1] * f[2][0] - f[0][0] * f[2][1],
        ],
        [
            f[0][1] * f[1][2] - f[0][2] * f[1][1],
            f[0][2] * f[1][0] - f[0][0] * f[1][2],
            f[0][0] * f[1][1] - f[0][1] * f[1][0],
        ],
    ]
}

fn gradient(u: &[f64], t: &[usize; 4], g: &[[f64; 3]; 4]) -> Mat3 {
    let mut h = [[0.0; 3]; 3];
    for (a, node) in t.iter().enumerate() {
        for (i, row) in h.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v += u[3 * node + i] * g[a][j];
            }
        }
    }
    h
}

impl SolidCarrier {

    pub fn new(
        points: Arc<PushforwardPoints>,
        map: VoxelDesignMap,
        blocking: BlockingMap,
        scale: f64,
    ) -> CaeResult<Self> {
        if map.elements() != points.element_count() {
            return Err(CaeError::contract(
                "solid carrier: the voxel design map must cover the push-forward mesh's tetrahedra",
            ));
        }
        if !(scale.is_finite() && (1.0..=4.0).contains(&scale)) {
            return Err(CaeError::contract("solid carrier: blocking_scale must lie in [1, 4]"));
        }
        Ok(Self { points, map, blocking, scale, constant: false, geometry: None })
    }


    pub fn with_geometry(mut self, mesh: &TetMesh) -> CaeResult<Self> {
        if !self.points.matches_geometry(mesh) {
            return Err(CaeError::contract("solid carrier: the geometry must be the push-forward mesh"));
        }
        let zero = vec![0.0; self.points.trace_size()];
        let weights =
            self.points.weights(&zero, &vec![1.0; mesh.elements.len()], &BlockingMap::identity())?;
        self.geometry = Some(Arc::new(Geometry {
            elements: mesh.elements.clone(),
            gradients: mesh.gradients.clone(),
            weights,
        }));
        Ok(self)
    }

    #[must_use]
    pub fn with_constant_blocking(mut self) -> Self {
        self.constant = true;
        self.blocking = BlockingMap::identity();
        self
    }

    #[must_use]
    pub fn constant_blocking(&self) -> bool {
        self.constant
    }

    #[must_use]
    pub fn pushforward_points(&self) -> &Arc<PushforwardPoints> {
        &self.points
    }

    #[must_use]
    pub fn blocking(&self) -> BlockingMap {
        self.blocking
    }

    #[must_use]
    pub fn scale(&self) -> f64 {
        self.scale
    }


    pub fn skipped_void_elements(&self, trace: &[f64], design: &[f64]) -> CaeResult<usize> {
        self.check(trace, design)?;
        let rho = self.element_density(design)?;
        Ok(self.points.skipped_void_elements(trace, &rho, &self.blocking)?.len())
    }

    fn element_density(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        if self.constant { Ok(vec![1.0; self.map.elements()]) } else { self.map.forward(design) }
    }

    fn check(&self, trace: &[f64], design: &[f64]) -> CaeResult<()> {
        if trace.len() != self.points.trace_size() || design.len() != self.map.voxels() {
            return Err(CaeError::contract(format!(
                "solid carrier: trace of length {} and design of length {} required (got {} and {})",
                self.points.trace_size(),
                self.map.voxels(),
                trace.len(),
                design.len()
            )));
        }
        Ok(())
    }
}

impl LagrangianCarrier for SolidCarrier {
    fn trace_size(&self) -> usize {
        self.points.trace_size()
    }

    fn design_size(&self) -> usize {
        self.map.voxels()
    }

    fn point_count(&self) -> usize {
        self.points.len()
    }

    fn points(&self, trace: &[f64], design: &[f64]) -> CaeResult<PointCloud> {
        self.check(trace, design)?;
        let rho = self.element_density(design)?;
        let positions = self.points.positions(trace)?;
        let mut weights = self.points.weights(trace, &rho, &self.blocking)?;
        for w in &mut weights {
            *w *= self.scale;
        }
        Ok(PointCloud { positions, weights })
    }

    fn points_jvp(
        &self,
        trace: &[f64],
        design: &[f64],
        d_trace: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<PointCloud> {
        self.check(trace, design)?;
        if d_trace.len() != trace.len() {
            return Err(CaeError::contract("solid carrier: trace direction has the wrong length"));
        }
        let rho = self.element_density(design)?;
        let drho = match d_design {
            Some(_) if self.constant => vec![0.0; rho.len()],
            Some(d) => {
                if d.len() != design.len() {
                    return Err(CaeError::contract("solid carrier: design direction has the wrong length"));
                }
                self.map.forward(d)?
            }
            None => vec![0.0; rho.len()],
        };
        let positions = self.points.positions_jvp(d_trace)?;
        let mut weights = self.points.weights_jvp(trace, &rho, &self.blocking, d_trace, &drho)?;
        for w in &mut weights {
            *w *= self.scale;
        }
        Ok(PointCloud { positions, weights })
    }

    fn points_vjp(
        &self,
        trace: &[f64],
        design: &[f64],
        positions_bar: &[[f64; 3]],
        weights_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.check(trace, design)?;
        let rho = self.element_density(design)?;
        let mut trace_bar = self.points.positions_vjp(positions_bar)?;
        let scaled: Vec<f64> = weights_bar.iter().map(|w| w * self.scale).collect();
        let (u_bar, rho_bar) = self.points.weights_vjp(trace, &rho, &self.blocking, &scaled)?;
        for (a, b) in trace_bar.iter_mut().zip(&u_bar) {
            *a += b;
        }
        if self.constant {
            return Ok((trace_bar, vec![0.0; self.map.voxels()]));
        }
        Ok((trace_bar, self.map.pullback(&rho_bar)?))
    }

    fn impulses_to_flux(&self, impulses: &[[f64; 3]], dt: f64) -> Vec<f64> {
        match self.points.nodal_force(impulses, dt) {
            Ok(f) => f,
            Err(_) => vec![f64::NAN; self.points.trace_size()],
        }
    }

    fn flux_to_impulses(&self, flux: &[f64], dt: f64) -> Vec<[f64; 3]> {
        match self.points.positions_jvp(flux) {
            Ok(v) if dt.is_finite() && dt > 0.0 => v.into_iter().map(|p| p.map(|x| x / dt)).collect(),
            _ => vec![[f64::NAN; 3]; self.points.len()],
        }
    }

    fn second_order(&self) -> Option<&dyn SecondOrderCarrier> {
        self.geometry.is_some().then_some(self as &dyn SecondOrderCarrier)
    }
}

impl SecondOrderCarrier for SolidCarrier {
    fn points_vjp_tangent(
        &self,
        trace: &[f64],
        design: &[f64],
        _positions_bar: &[[f64; 3]],
        weights_bar: &[f64],
        d_trace: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {

        let geo = self.geometry.as_ref().ok_or_else(|| {
            CaeError::contract("solid carrier: no element geometry (second order unavailable)")
        })?;
        self.check(trace, design)?;
        if d_trace.len() != trace.len() || weights_bar.len() != self.points.len() {
            return Err(CaeError::contract("solid carrier: tangent direction or cotangent length mismatch"));
        }
        let rho = self.element_density(design)?;
        let drho = match d_design {
            Some(d) if !self.constant => {
                if d.len() != design.len() {
                    return Err(CaeError::contract("solid carrier: design direction has the wrong length"));
                }
                self.map.forward(d)?
            }
            _ => vec![0.0; rho.len()],
        };
        let threshold = self.points.void_threshold();
        let mut trace_dot = vec![0.0; trace.len()];
        let mut rho_dot = vec![0.0; rho.len()];
        for (e, t) in geo.elements.iter().enumerate() {
            let s: f64 = self.points.element_points(e).map(|q| weights_bar[q] * geo.weights[q]).sum::<f64>()
                * self.scale;
            if s == 0.0 {
                continue;
            }
            let g = &geo.gradients[e];
            let h = gradient(trace, t, g);
            let mut f = h;
            for (i, row) in f.iter_mut().enumerate() {
                row[i] += 1.0;
            }
            let cof = cofactor(&f);
            let j: f64 = (0..3).map(|k| f[0][k] * cof[0][k]).sum();
            let [b, db, ddb] = if self.constant {
                [1.0, 0.0, 0.0]
            } else {
                let v = self.blocking.value(HyperDual::new(rho[e], 1.0, 1.0, 0.0));
                [v.re, v.e1, v.e12]
            };
            if j <= 0.0 && b <= threshold {

                continue;
            }
            let df = gradient(d_trace, t, g);
            let (sum, dd) = {
                let mut fs = f;
                for i in 0..3 {
                    for k in 0..3 {
                        fs[i][k] += df[i][k];
                    }
                }
                (cofactor(&fs), cofactor(&df))
            };
            let mut dcof = [[0.0; 3]; 3];
            for i in 0..3 {
                for k in 0..3 {
                    dcof[i][k] = sum[i][k] - cof[i][k] - dd[i][k];
                }
            }
            let dj: f64 =
                (0..3).flat_map(|i| (0..3).map(move |k| (i, k))).map(|(i, k)| cof[i][k] * df[i][k]).sum();
            let dr = drho[e];
            for (a, node) in t.iter().enumerate() {
                for i in 0..3 {
                    let jd: f64 = (0..3).map(|k| cof[i][k] * g[a][k]).sum();
                    let jdd: f64 = (0..3).map(|k| dcof[i][k] * g[a][k]).sum();
                    trace_dot[3 * node + i] += s * (db * dr * jd + b * jdd);
                }
            }
            rho_dot[e] = s * (ddb * dr * j + db * dj);
        }
        if self.constant {
            return Ok((trace_dot, vec![0.0; self.map.voxels()]));
        }
        Ok((trace_dot, self.map.pullback(&rho_dot)?))
    }
}
