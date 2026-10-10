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


use implexity_core::CaeError;
use implexity_linalg::sparse::CsrMatrix;



pub fn cell_center_temperature_map(grid: &[i64], node_ijk: &[[i64; 3]]) -> Result<CsrMatrix, CaeError> {
    if grid.len() != 3 || grid.iter().any(|n| *n < 1) {
        return Err(CaeError::contract("temperature restriction requires a positive Cartesian cell grid"));
    }
    let shape: Vec<usize> = grid.iter().map(|n| usize::try_from(*n).unwrap_or(0)).collect();
    let nshape = [shape[0] + 1, shape[1] + 1, shape[2] + 1];
    let nnodes = nshape[0] * nshape[1] * nshape[2];
    let expected = (0..nnodes).all(|i| {
        let ijk = [i / (nshape[1] * nshape[2]), (i / nshape[2]) % nshape[1], i % nshape[2]];
        node_ijk.get(i).is_some_and(|p| (0..3).all(|k| usize::try_from(p[k]).ok() == Some(ijk[k])))
    });
    if node_ijk.len() != nnodes || !expected {
        return Err(CaeError::contract(
            "temperature restriction requires the declared C-order Cartesian node coordinates",
        ));
    }
    let ncells = shape[0] * shape[1] * shape[2];
    let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
    for c in 0..ncells {
        let cell = [c / (shape[1] * shape[2]), (c / shape[2]) % shape[1], c % shape[2]];
        for corner in 0..8 {
            let v = [cell[0] + (corner >> 2 & 1), cell[1] + (corner >> 1 & 1), cell[2] + (corner & 1)];
            rows.push(c);
            cols.push((v[0] * nshape[1] + v[1]) * nshape[2] + v[2]);
            vals.push(0.125);
        }
    }
    let q = CsrMatrix::from_triplets(ncells, nnodes, &rows, &cols, &vals)
        .map_err(|e| CaeError::contract(e.to_string()))?;
    let ones = q.matvec(&vec![1.0; nnodes]).map_err(|e| CaeError::contract(e.to_string()))?;
    let mut ok = ones.iter().all(|v| (*v - 1.0).abs() == 0.0);
    for k in 0..3 {
        let coord: Vec<f64> = node_ijk.iter().map(|p| p[k] as f64).collect();
        let centre = q.matvec(&coord).map_err(|e| CaeError::contract(e.to_string()))?;
        for (c, value) in centre.iter().enumerate() {
            let cell = [c / (shape[1] * shape[2]), (c / shape[2]) % shape[1], c % shape[2]];
            ok &= (*value - (cell[k] as f64 + 0.5)).abs() == 0.0;
        }
    }
    if !ok {
        return Err(CaeError::contract("temperature restriction failed its affine reproduction contract"));
    }
    Ok(q)
}
