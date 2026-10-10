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
