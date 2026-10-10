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


pub fn nonnegative_least_squares(a: &[Vec<f64>], b: &[f64]) -> Result<Vec<f64>, crate::LinalgError> {
    if a.len() != b.len()
        || a.is_empty()
        || a.iter().any(|r| r.len() != a[0].len() || r.iter().any(|v| !v.is_finite()))
        || b.iter().any(|v| !v.is_finite())
    {
        return Err(crate::LinalgError::Shape("nonnegative least-squares input shape or values".into()));
    }
    let n = a.first().map_or(0, Vec::len);
    let mut ata = vec![vec![0.0; n]; n];
    let mut atb = vec![0.0; n];
    for (row, bi) in a.iter().zip(b) {
        for i in 0..n {
            atb[i] += row[i] * bi;
            for j in 0..n {
                ata[i][j] += row[i] * row[j];
            }
        }
    }
    let mut x = vec![0.0; n];
    for _ in 0..20_000 {
        let mut change = 0.0_f64;
        for i in 0..n {
            if ata[i][i] <= 0.0 {
                continue;
            }
            let g: f64 = (0..n).map(|j| ata[i][j] * x[j]).sum::<f64>() - atb[i];
            let next = (x[i] - g / ata[i][i]).max(0.0);
            change = change.max((next - x[i]).abs());
            x[i] = next;
        }
        if change < 1e-14 {
            break;
        }
    }
    Ok(x)
}
