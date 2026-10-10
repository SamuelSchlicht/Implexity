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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrackTopologyState {
    pub open_edges: Vec<bool>,
    pub components: Vec<Vec<usize>>,
    pub topology_changed: bool,
}

fn components(n: usize, edges: &[[usize; 2]], open: &[bool]) -> Vec<Vec<usize>> {
    let mut adj = vec![Vec::new(); n];
    for (k, &[a, b]) in edges.iter().enumerate() {
        if open[k] {
            continue;
        }
        adj[a].push(b);
        adj[b].push(a);
    }
    let mut seen = vec![false; n];
    let mut out = Vec::new();
    for s in 0..n {
        if seen[s] {
            continue;
        }
        let mut stack = vec![s];
        seen[s] = true;
        let mut c = Vec::new();
        while let Some(u) = stack.pop() {
            c.push(u);
            for &v in &adj[u] {
                if !seen[v] {
                    seen[v] = true;
                    stack.push(v);
                }
            }
        }
        c.sort_unstable();
        out.push(c);
    }
    out
}



pub fn update_crack_topology(
    node_count: usize,
    edges: &[[usize; 2]],
    old_open: &[bool],
    driving: &[f64],
    threshold: f64,
    margin: f64,
) -> CaeResult<CrackTopologyState> {
    if old_open.len() != driving.len()
        || old_open.len() != edges.len()
        || edges.iter().flatten().any(|&i| i >= node_count)
    {
        return Err(CaeError::contract("edge-state mismatch"));
    }
    if driving.iter().any(|d| (d - threshold).abs() <= margin) {
        return Err(CaeError::contract("crack nucleation boundary requires event localization"));
    }
    let open: Vec<bool> = old_open.iter().zip(driving).map(|(o, d)| *o || *d > threshold).collect();
    let changed = open.iter().zip(old_open).any(|(a, b)| a != b);
    Ok(CrackTopologyState {
        components: components(node_count, edges, &open),
        open_edges: open,
        topology_changed: changed,
    })
}

#[must_use]
pub fn crack_event_fraction(d0: &[f64], d1: &[f64], threshold: f64) -> Option<f64> {
    d0.iter()
        .zip(d1)
        .filter(|(a, b)| **a < threshold && **b >= threshold && *b - *a > 0.0)
        .map(|(a, b)| (threshold - a) / (b - a))
        .reduce(f64::min)
}

