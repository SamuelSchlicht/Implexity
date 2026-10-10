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

use crate::gas_vjp::{ReverseScalar, scoped_pullback};
use crate::native_gas_port_coupling::NativeGasPortConnector;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::lattice::Lattice;

pub struct NativeGasMomentIncomingBars {
    pub incoming_rows: Vec<(usize, Vec<f64>)>,
    pub packet_moments: Vec<[f64; 10]>,
    pub reflection_moments: Vec<(usize, [f64; 10])>,
    pub velocity_offsets: Vec<[f64; 3]>,
}

pub fn incoming_moments_vjp<const Q: usize, L: Lattice<Q>>(
    owner: &NativeGasPortConnector,
    rows: &[(usize, Vec<f64>)],
    packets: &[[f64; 10]],
    reflections: &[(usize, [f64; 10])],
    offsets: &[[f64; 3]],
    row_bar: &[(usize, Vec<f64>)],
    defect_bar: [f64; 10],
    heat_bar: f64,
) -> CaeResult<NativeGasMomentIncomingBars> {
    owner.replace_incoming_moments::<f64, Q, L>(rows, packets, reflections, offsets)?;
    if row_bar.len() != rows.len()
        || row_bar.iter().zip(rows).any(|((i, b), (j, r))| {
            i != j || b.len() != r.len() || b.iter().any(|v| !v.is_finite())
        })
        || defect_bar.iter().chain([&heat_bar]).any(|v| !v.is_finite())
    {
        return Err(CaeError::contract(
            "native moment return cotangent shape or value",
        ));
    }
    let mut values: Vec<f64> = rows.iter().flat_map(|r| r.1.iter().copied()).collect();
    values.extend(packets.iter().flatten().copied());
    values.extend(reflections.iter().flat_map(|r| r.1.iter().copied()));
    values.extend(offsets.iter().flatten().copied());
    let bars = scoped_pullback(&values, |variables| {
        let mut cursor = 0;
        let mut r = Vec::with_capacity(rows.len());
        for (cell, row) in rows {
            r.push((*cell, variables[cursor..cursor + row.len()].to_vec()));
            cursor += row.len();
        }
        let p: Vec<[ReverseScalar; 10]> = (0..packets.len())
            .map(|_| {
                let v = std::array::from_fn(|i| variables[cursor + i]);
                cursor += 10;
                v
            })
            .collect();
        let f: Vec<(usize, [ReverseScalar; 10])> = reflections
            .iter()
            .map(|(cell, _)| {
                let v = std::array::from_fn(|i| variables[cursor + i]);
                cursor += 10;
                (*cell, v)
            })
            .collect();
        let o: Vec<[ReverseScalar; 3]> = (0..offsets.len())
            .map(|_| {
                let v = std::array::from_fn(|i| variables[cursor + i]);
                cursor += 3;
                v
            })
            .collect();
        let out = owner.replace_incoming_moments::<ReverseScalar, Q, L>(&r, &p, &f, &o)?;
        let mut value = out.projection_heat_j * heat_bar;
        for (output, bar) in out.incoming_rows.iter().zip(row_bar) {
            for (v, b) in output.1.iter().zip(&bar.1) {
                value += *v * *b;
            }
        }
        for (v, b) in out.exchange_moment_defect.iter().zip(defect_bar) {
            value += *v * b;
        }
        Ok(value)
    })?;
    let mut cursor = 0;
    let incoming_rows = rows
        .iter()
        .map(|(cell, row)| {
            let v = bars[cursor..cursor + row.len()].to_vec();
            cursor += row.len();
            (*cell, v)
        })
        .collect();
    let packet_moments = (0..packets.len())
        .map(|_| {
            let v = std::array::from_fn(|i| bars[cursor + i]);
            cursor += 10;
            v
        })
        .collect();
    let reflection_moments = reflections
        .iter()
        .map(|(cell, _)| {
            let v = std::array::from_fn(|i| bars[cursor + i]);
            cursor += 10;
            (*cell, v)
        })
        .collect();
    let velocity_offsets = (0..offsets.len())
        .map(|_| {
            let v = std::array::from_fn(|i| bars[cursor + i]);
            cursor += 3;
            v
        })
        .collect();
    Ok(NativeGasMomentIncomingBars {
        incoming_rows,
        packet_moments,
        reflection_moments,
        velocity_offsets,
    })
}
