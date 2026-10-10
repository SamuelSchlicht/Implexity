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

use crate::gas_mixed_network::{MixedGasLayout, MixedGasLocalPlan, MixedGasPolicy};
use crate::gas_multiscale_network::{MultiscaleNetworkInputs, MultiscaleNetworkTopology};
use crate::gas_vjp::{ReverseScalar, scoped_pullback};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

pub struct MixedGasOutputBars {
    pub inventory_residual: Vec<f64>,
    pub face_packets: Vec<[f64; 7]>,
}
pub struct MixedGasInputBars {
    pub current_inventory: Vec<f64>,
    pub previous_inventory: Vec<f64>,
    pub port_density_kg_m3: Vec<Vec<f64>>,
}
enum Input {
    Current(usize),
    Previous(usize),
    Port(usize, usize),
}
pub fn fixed_state_output_vjp(
    a: &MultiscaleNetworkInputs<f64>,
    layout: &MixedGasLayout,
    current: &[f64],
    previous: &[f64],
    bars: &MixedGasOutputBars,
    policy: &MixedGasPolicy,
) -> CaeResult<MixedGasInputBars> {
    let n = a.gas.volume_m3.len();
    let nv = layout.population_count();
    if current.len() != layout.dimension()
        || previous.len() != layout.dimension()
        || bars.inventory_residual.len() != layout.dimension()
        || bars.face_packets.len() != a.gas.faces.len()
        || current
            .iter()
            .chain(previous)
            .chain(&bars.inventory_residual)
            .chain(bars.face_packets.iter().flatten())
            .any(|v| !v.is_finite())
    {
        return Err(CaeError::contract("mixed output transpose shape or value"));
    }
    let topology = MultiscaleNetworkTopology::new_with_policy(a, &policy.network)?;
    let oldmoments = layout.moments(previous, &a.gas.grid.velocity_m_s)?;
    let mut column_owner = vec![usize::MAX; layout.dimension()];
    for cell in 0..n {
        if let Some(o) = layout.cell_offset(cell) {
            let count = if layout.continuum_cells()[cell] {
                4
            } else {
                nv
            };
            column_owner[o..o + count].fill(cell);
        }
    }
    let mut out = MixedGasInputBars {
        current_inventory: vec![0.; layout.dimension()],
        previous_inventory: vec![0.; layout.dimension()],
        port_density_kg_m3: a
            .gas
            .port_density_kg_m3
            .iter()
            .map(|v| vec![0.; v.len()])
            .collect(),
    };
    for cell in 0..n {
        let Some(o) = layout.cell_offset(cell) else {
            continue;
        };
        if a.gas.volume_m3[cell] == 0.0 {
            continue;
        }
        let count = if layout.continuum_cells()[cell] {
            4
        } else {
            nv
        };
        let packet_active = topology.incident_faces[cell].iter().any(|&fi| {
            a.gas.faces[fi].left == cell && bars.face_packets[fi].iter().any(|v| *v != 0.)
        });
        if !packet_active
            && bars.inventory_residual[o..o + count]
                .iter()
                .all(|v| *v == 0.)
        {
            continue;
        }
        let plan = MixedGasLocalPlan::new_prepared(a, layout, policy, &topology, &[cell])?;
        let mut values = vec![];
        let mut inputs = vec![];
        let mut current_indices = vec![];
        let mut previous_indices = vec![];
        for &global in &plan.state_columns {
            let owner = column_owner[global];
            if owner == usize::MAX {
                return Err(CaeError::contract(
                    "mixed transpose column has no cell owner",
                ));
            }
            current_indices.push(if a.gas.volume_m3[owner] > 0.0 {
                let id = values.len();
                values.push(current[global]);
                inputs.push(Input::Current(global));
                Some(id)
            } else {
                None
            });
            previous_indices.push(if oldmoments[owner][0] > 0.0 {
                let id = values.len();
                values.push(previous[global]);
                inputs.push(Input::Previous(global));
                Some(id)
            } else {
                None
            });
        }
        let mut port_indices = vec![];
        for &port in &plan.port_columns {
            let mut ids = vec![];
            for k in 0..nv {
                ids.push(values.len());
                values.push(a.gas.port_density_kg_m3[port][k]);
                inputs.push(Input::Port(port, k));
            }
            port_indices.push(ids);
        }
        let pulled = scoped_pullback(&values, |variables| {
            let c = plan
                .state_columns
                .iter()
                .zip(&current_indices)
                .map(|(&g, i)| i.map_or(ReverseScalar::from_f64(current[g]), |i| variables[i]))
                .collect::<Vec<_>>();
            let p = plan
                .state_columns
                .iter()
                .zip(&previous_indices)
                .map(|(&g, i)| i.map_or(ReverseScalar::from_f64(previous[g]), |i| variables[i]))
                .collect::<Vec<_>>();
            let ports = port_indices
                .iter()
                .map(|v| v.iter().map(|&i| variables[i]).collect())
                .collect();
            let result = plan.evaluate_with_ports(&c, &p, ports)?;
            let mut total = ReverseScalar::zero();
            for &(global, local) in &plan.output_columns {
                total += result.inventory_residual[local] * bars.inventory_residual[global];
            }
            for &(global, local) in &plan.face_columns {
                if a.gas.faces[global].left != cell {
                    continue;
                }
                let q = &result.face_packets[local];
                let packet = [
                    q.mass_kg,
                    q.momentum_kg_m_s[0],
                    q.momentum_kg_m_s[1],
                    q.momentum_kg_m_s[2],
                    q.kinetic_energy_j,
                    q.wall_work_j,
                    q.wall_heat_j,
                ];
                for k in 0..7 {
                    total += packet[k] * bars.face_packets[global][k];
                }
            }
            Ok(total)
        })?;
        for (input, bar) in inputs.into_iter().zip(pulled) {
            match input {
                Input::Current(i) => out.current_inventory[i] += bar,
                Input::Previous(i) => out.previous_inventory[i] += bar,
                Input::Port(p, k) => out.port_density_kg_m3[p][k] += bar,
            }
        }
    }
    Ok(out)
}
