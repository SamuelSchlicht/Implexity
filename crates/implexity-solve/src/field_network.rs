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


use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;

fn err(message: &str) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldPort {
    pub name: String,
    pub field_indices: Vec<i64>,
    pub weights: Vec<f64>,
    pub orientation: f64,
    pub conserved_quantity: String,
}

impl FieldPort {


    pub fn new(
        name: &str,
        field_indices: Vec<i64>,
        weights: Vec<f64>,
        orientation: f64,
        conserved_quantity: &str,
    ) -> CaeResult<Self> {
        if field_indices.len() != weights.len() || field_indices.is_empty() {
            return Err(err("field_indices and weights must be non-empty and equally sized"));
        }
        if !orientation.is_finite() || orientation == 0.0 {
            return Err(err("orientation must be finite and non-zero"));
        }
        Ok(Self {
            name: name.into(),
            field_indices,
            weights,
            orientation,
            conserved_quantity: conserved_quantity.into(),
        })
    }

    fn checked(&self, size: usize) -> CaeResult<(Vec<usize>, f64)> {
        let idx: Vec<usize> = self
            .field_indices
            .iter()
            .map(|&i| usize::try_from(i).ok().filter(|&i| i < size))
            .collect::<Option<_>>()
            .ok_or_else(|| err("field port index out of bounds"))?;
        let sw: f64 = self.weights.iter().sum();
        Ok((idx, sw))
    }
}



pub fn sample_field(field: &[f64], port: &FieldPort) -> CaeResult<f64> {
    let (idx, sw) = port.checked(field.len())?;
    if sw.abs() <= f64::MIN_POSITIVE {
        return Err(err("field port weights sum to zero"));
    }
    Ok(idx.iter().zip(&port.weights).map(|(&i, w)| w * field[i]).sum::<f64>() / sw)
}



pub fn scatter_port(value: f64, size: usize, port: &FieldPort) -> CaeResult<Vec<f64>> {
    let (idx, sw) = port.checked(size)?;
    let mut out = vec![0.0; size];
    for (&i, w) in idx.iter().zip(&port.weights) {
        out[i] += value * w / sw;
    }
    Ok(out)
}



pub fn port_adjoint_identity(field: &[f64], port: &FieldPort, scalar: f64) -> CaeResult<f64> {
    let lhs = sample_field(field, port)? * scalar;
    let rhs: f64 = field.iter().zip(scatter_port(scalar, field.len(), port)?).map(|(a, b)| a * b).sum();
    Ok(lhs - rhs)
}



pub fn incidence_matrix(edges: &[(i64, i64)], node_count: usize) -> CaeResult<DenseMatrix> {
    let mut b = DenseMatrix::zeros(node_count, edges.len());
    for (j, &(a, h)) in edges.iter().enumerate() {
        let (Some(a), Some(h)) = (
            usize::try_from(a).ok().filter(|&a| a < node_count),
            usize::try_from(h).ok().filter(|&h| h < node_count),
        ) else {
            return Err(err("invalid network edge"));
        };
        if a == h {
            return Err(err("invalid network edge"));
        }
        b.data[a * edges.len() + j] -= 1.0;
        b.data[h * edges.len() + j] += 1.0;
    }
    Ok(b)
}



pub fn network_conservation_residual(
    edge_fluxes: &[f64],
    edges: &[(i64, i64)],
    node_sources: &[f64],
    node_count: Option<usize>,
) -> CaeResult<Vec<f64>> {
    let n = node_count.unwrap_or(node_sources.len());
    if node_sources.len() != n {
        return Err(err("node source count mismatch"));
    }
    let b = incidence_matrix(edges, n)?;
    if edge_fluxes.len() != b.ncols {
        return Err(err("edge flux count mismatch"));
    }
    let bq = b.matvec(edge_fluxes).map_err(|e| err(&e.to_string()))?;
    Ok(bq.iter().zip(node_sources).map(|(a, s)| a + s).collect())
}

