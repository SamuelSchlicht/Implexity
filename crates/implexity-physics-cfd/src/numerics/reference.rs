// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::rng::default_rng;
use implexity_linalg::CsrMatrix;
use serde_json::{Map, json};

use crate::error::{CfdError, CfdResult};
use crate::numerics::system::SaddlePointSystem;

#[derive(Debug, Clone)]
pub struct GridFluxReferenceModel {
    pub shape: [usize; 3],
    pub full_divergence: CsrMatrix,
    pub gauged_divergence: CsrMatrix,
    pub gauge_cell: usize,
    pub viscosity_scale: f64,
    pub alpha_fluid: f64,
    pub alpha_solid: f64,
    pub penalization: f64,
    pub velocity_rhs: Vec<f64>,
}

impl GridFluxReferenceModel {
    #[must_use]
    pub fn n_velocity(&self) -> usize {
        self.full_divergence.ncols()
    }

    #[must_use]
    pub fn n_cells(&self) -> usize {
        self.full_divergence.nrows()
    }

    #[must_use]
    pub fn n_pressure(&self) -> usize {
        self.n_cells() - 1
    }

    fn check_control(&self, s: &[f64]) -> CfdResult<()> {
        if s.len() != self.n_velocity() || s.iter().any(|v| !(0.0..=1.0).contains(v)) {
            return Err(CfdError::Contract(
                "model:control must contain one value in [0, 1] per flux degree of freedom.".into(),
            ));
        }
        Ok(())
    }


    pub fn alpha(&self, s: &[f64]) -> CfdResult<Vec<f64>> {
        self.check_control(s)?;
        Ok(s.iter()
            .map(|v| {
                self.alpha_fluid + (self.alpha_solid - self.alpha_fluid) * (1.0 - v).powf(self.penalization)
            })
            .collect())
    }

    #[must_use]
    pub fn alpha_derivative(&self, s: &[f64]) -> Vec<f64> {
        s.iter()
            .map(|v| {
                -self.penalization
                    * (self.alpha_solid - self.alpha_fluid)
                    * (1.0 - v).powf(self.penalization - 1.0)
            })
            .collect()
    }


    pub fn system(&self, s: &[f64]) -> CfdResult<SaddlePointSystem> {
        let alpha = self.alpha(s)?;
        let n = self.n_velocity();
        let dt = self.full_divergence.transpose();
        let laplacian = dt.matmul(&self.full_divergence)?;
        let shifted = laplacian.add_scaled(1.0, &CsrMatrix::identity(n), 0.25)?;
        let a = shifted.add_scaled(self.viscosity_scale, &CsrMatrix::diagonal_matrix(&alpha), 1.0)?;
        let mut metadata = Map::new();
        metadata.insert("verification_only".into(), json!(true));
        metadata.insert("topology_coordinate".into(), json!("model:control"));
        metadata.insert("shape".into(), json!(self.shape));
        metadata.insert("pressure_gauge_cell".into(), json!(self.gauge_cell));
        SaddlePointSystem::new(
            a,
            self.gauged_divergence.clone(),
            None,
            Some(&self.velocity_rhs),
            Some(&vec![0.0; self.n_pressure()]),
            metadata,
        )
    }


    pub fn full_mass_residual(&self, velocity: &[f64]) -> CfdResult<Vec<f64>> {
        Ok(self.full_divergence.matvec(velocity)?)
    }

    #[must_use]
    pub fn residual_design_vjp(&self, adjoint: &[f64], state: &[f64], s: &[f64]) -> Vec<f64> {
        let n = self.n_velocity();
        let d = self.alpha_derivative(s);
        (0..n).map(|i| adjoint[i] * d[i] * state[i]).collect()
    }
}


pub fn build_grid_flux_reference_model(
    shape: [usize; 3],
    viscosity_scale: f64,
    alpha_fluid: f64,
    alpha_solid: f64,
    penalization: f64,
    gauge_cell: usize,
    seed: u128,
) -> CfdResult<GridFluxReferenceModel> {
    let [nx, ny, nz] = shape;
    if nx.min(ny).min(nz) < 2 {
        return Err(CfdError::Contract(
            "The three-dimensional reference grid requires at least two cells per axis.".into(),
        ));
    }
    let n_cells = nx * ny * nz;
    if gauge_cell >= n_cells {
        return Err(CfdError::Contract("The pressure-gauge cell is outside the grid.".into()));
    }
    let cell = |i: usize, j: usize, k: usize| i + nx * (j + ny * k);
    let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
    let mut edge = 0usize;
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let a = cell(i, j, k);
                for (di, dj, dk) in [(1, 0, 0), (0, 1, 0), (0, 0, 1)] {
                    let (ii, jj, kk) = (i + di, j + dj, k + dk);
                    if ii < nx && jj < ny && kk < nz {
                        let b = cell(ii, jj, kk);
                        rows.extend([a, b]);
                        cols.extend([edge, edge]);
                        vals.extend([-1.0, 1.0]);
                        edge += 1;
                    }
                }
            }
        }
    }
    let full = CsrMatrix::from_triplets(n_cells, edge, &rows, &cols, &vals)?;
    let (mut gr, mut gc, mut gv) = (Vec::new(), Vec::new(), Vec::new());
    let mut out_row = 0usize;
    for r in 0..n_cells {
        if r == gauge_cell {
            continue;
        }
        let (ci, vi) = full.row(r);
        for (c, v) in ci.iter().zip(vi) {
            gr.push(out_row);
            gc.push(*c);
            gv.push(*v);
        }
        out_row += 1;
    }
    let gauged = CsrMatrix::from_triplets(n_cells - 1, edge, &gr, &gc, &gv)?;
    let mut rng = default_rng(seed);
    let mut forcing: Vec<f64> = (0..edge).map(|_| rng.standard_normal()).collect();
    let norm = forcing.iter().map(|v| v * v).sum::<f64>().sqrt().max(f64::MIN_POSITIVE);
    for v in &mut forcing {
        *v /= norm;
    }
    Ok(GridFluxReferenceModel {
        shape,
        full_divergence: full,
        gauged_divergence: gauged,
        gauge_cell,
        viscosity_scale,
        alpha_fluid,
        alpha_solid,
        penalization,
        velocity_rhs: forcing,
    })
}
