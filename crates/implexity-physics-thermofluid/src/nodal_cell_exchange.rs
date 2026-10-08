// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_linalg::CsrMatrix;
use implexity_physics_base::model_errors::{PhysicsError, PhysicsResult};


#[derive(Debug, Clone, PartialEq)]
pub struct NodalCellExchange {
    rows: Vec<usize>,
    columns: Vec<usize>,
    weights: Vec<f64>,
    cell_count: usize,
    node_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExchangePowers<S> {
    pub fluid_cell_power_w: Vec<S>,
    pub solid_nodal_power_w: Vec<S>,
    pub fluid_outgoing_conductance_w_k: Vec<S>,
    pub fluid_reservoir_power_w: Vec<S>,
    pub interface_temperature_jump_k: Vec<S>,
}

impl NodalCellExchange {

    pub fn new(
        rows: Vec<usize>,
        columns: Vec<usize>,
        weights: Vec<f64>,
        cell_count: usize,
        node_count: usize,
    ) -> PhysicsResult<Self> {
        if cell_count < 1 || node_count < 1 {
            return Err(PhysicsError::value("positive exact interface dimensions required"));
        }
        if columns.len() != rows.len()
            || weights.len() != rows.len()
            || weights.iter().any(|w| !w.is_finite() || *w < 0.0)
            || rows.iter().any(|r| *r >= cell_count)
            || columns.iter().any(|c| *c >= node_count)
        {
            return Err(PhysicsError::value("invalid sparse interface interpolation entries"));
        }
        let mut total = vec![0.0; cell_count];
        for (r, w) in rows.iter().zip(&weights) {
            total[*r] += w;
        }
        if total.iter().any(|t| (t - 1.0).abs() > 1e-12) {
            return Err(PhysicsError::value("interface interpolation must preserve constants"));
        }
        Ok(Self { rows, columns, weights, cell_count, node_count })
    }


    pub fn from_temperature_map(h: &CsrMatrix) -> PhysicsResult<Self> {
        let (n_cells, n_nodes) = h.shape();
        if n_cells.min(n_nodes) < 1 || h.data().iter().any(|v| !v.is_finite() || *v < 0.0) {
            return Err(PhysicsError::value("finite nonnegative nonempty temperature map required"));
        }
        let (mut rows, mut columns, mut weights) = (Vec::new(), Vec::new(), Vec::new());
        for r in 0..n_cells {
            let (cols, vals) = h.row(r);
            let sum: f64 = vals.iter().sum();
            if (sum - 1.0).abs() > 1e-12 {
                return Err(PhysicsError::value(
                    "temperature map must preserve constants; no normalization performed",
                ));
            }
            for (c, v) in cols.iter().zip(vals) {
                rows.push(r);
                columns.push(*c);
                weights.push(*v);
            }
        }
        Self::new(rows, columns, weights, n_cells, n_nodes)
    }

    #[must_use]
    pub fn rows(&self) -> &[usize] {
        &self.rows
    }

    #[must_use]
    pub fn columns(&self) -> &[usize] {
        &self.columns
    }

    #[must_use]
    pub fn weights(&self) -> &[f64] {
        &self.weights
    }

    #[must_use]
    pub fn cell_count(&self) -> usize {
        self.cell_count
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.node_count
    }


    pub fn admit(
        &self,
        nodal_temperature_k: &[f64],
        cell_temperature_k: &[f64],
        conductance_w_k: &[f64],
    ) -> PhysicsResult<()> {
        for (v, n) in [
            (nodal_temperature_k, self.node_count),
            (cell_temperature_k, self.cell_count),
            (conductance_w_k, self.cell_count),
        ] {
            if v.len() != n || !v.iter().copied().all(f64::is_finite) {
                return Err(PhysicsError::value("finite exact-shaped interface data required"));
            }
        }
        if conductance_w_k.iter().any(|g| *g < 0.0) {
            return Err(PhysicsError::value("negative interface conductance"));
        }
        Ok(())
    }


    pub fn exchange<S: Scalar>(&self, ts: &[S], tf: &[S], g: &[S]) -> PhysicsResult<ExchangePowers<S>> {
        if ts.len() != self.node_count || tf.len() != self.cell_count || g.len() != self.cell_count {
            return Err(PhysicsError::value("exact interface shapes required"));
        }
        let mut sampled = vec![S::zero(); self.cell_count];
        for ((r, c), w) in self.rows.iter().zip(&self.columns).zip(&self.weights) {
            sampled[*r] += ts[*c] * *w;
        }
        let jump: Vec<S> = sampled.iter().zip(tf).map(|(s, t)| *s - *t).collect();
        let q: Vec<S> = g.iter().zip(&jump).map(|(g, j)| *g * *j).collect();
        let mut nodal = vec![S::zero(); self.node_count];
        for ((r, c), w) in self.rows.iter().zip(&self.columns).zip(&self.weights) {
            nodal[*c] += q[*r] * *w;
        }
        Ok(ExchangePowers {
            solid_nodal_power_w: nodal.into_iter().map(|v| -v).collect(),
            fluid_reservoir_power_w: g.iter().zip(&sampled).map(|(g, s)| *g * *s).collect(),
            fluid_outgoing_conductance_w_k: g.to_vec(),
            interface_temperature_jump_k: jump,
            fluid_cell_power_w: q,
        })
    }
}
