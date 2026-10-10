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


use serde_json::{Value, json};

use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::local_assembly::{AssemblyOptions, Incidence};

pub fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

pub type RealArray = (Vec<usize>, Vec<f64>);


pub fn real_array(value: &Value, label: &str) -> CaeResult<RealArray> {
    match implexity_physics_solid::util::real_array(value) {
        Some((shape, data)) if data.iter().all(|v| v.is_finite()) => Ok((shape, data)),
        _ => Err(err(format!(
            "{label} requires finite real numeric components, not booleans, strings or phasors"
        ))),
    }
}

#[must_use]
pub fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

#[must_use]
pub fn py_real(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

#[must_use]
pub fn grid_of(v: &Value) -> Option<[usize; 3]> {
    let a = v.as_array().filter(|a| a.len() == 3)?;
    let g: Vec<usize> = a
        .iter()
        .filter_map(|x| py_int(x).filter(|n| *n >= 1).and_then(|n| usize::try_from(n).ok()))
        .collect();
    (g.len() == 3).then(|| [g[0], g[1], g[2]])
}

#[must_use]
pub fn times_match(times: &RealArray, expected: &RealArray) -> bool {
    times.0.len() == 1
        && times.1.len() >= 2
        && times.1.windows(2).all(|w| w[1] > w[0])
        && times.0 == expected.0
        && times.1 == expected.1
}

#[must_use]
pub fn nonempty_text(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.trim().is_empty())
}


pub fn extend_coupling(
    base: &Value,
    physics: &[&str],
    edges: &[(&str, &str, &str, &str, &str)],
    loops: &[[&str; 2]],
    notes: &[String],
) -> CaeResult<Value> {
    let mut d = CouplingDeclaration::from_value(base).map_err(|e| err(e.0))?;
    d.active_physics.extend(physics.iter().map(|s| (*s).to_string()));
    for (source, target, quantity, mode, reason) in edges {
        d.edges.push(CouplingEdge::new(source, target, quantity, mode, true, reason).map_err(|e| err(e.0))?);
    }
    d.closed_loops.extend(loops.iter().map(|l| vec![l[0].to_string(), l[1].to_string()]));
    d.notes.extend(notes.iter().cloned());
    Ok(d.to_value())
}

#[must_use]
pub fn assembly_options(s: &SolidKernel) -> AssemblyOptions {
    let batch = s.p["assembly"]["batch_size"].as_u64().and_then(|b| usize::try_from(b).ok()).unwrap_or(64);
    AssemblyOptions { batch_size: batch.max(1), ..AssemblyOptions::default() }
}

#[must_use]
pub fn thermal_rows(s: &SolidKernel, offset: usize) -> Vec<i64> {
    let off = i64::try_from(offset).unwrap_or(0);
    s.mesh
        .tets
        .iter()
        .flat_map(|tet| tet.iter().map(move |n| s.tmap[*n]))
        .map(|r| if r < 0 { -1 } else { r + off })
        .collect()
}

#[must_use]
pub fn design_incidence(s: &SolidKernel) -> Vec<i64> {
    let nc = i64::try_from(s.nc).unwrap_or(0);
    s.mesh
        .owners
        .iter()
        .flat_map(|o| {
            let o = i64::try_from(*o).unwrap_or(0);
            [o, nc, nc + 1, nc + 2, nc + 3 + o]
        })
        .collect()
}


pub fn incidence(count: usize, width: usize, values: Vec<i64>) -> CaeResult<Incidence> {
    Incidence::new(count, width, values)
}

#[must_use]
pub fn zero_csr(rows: usize, cols: usize) -> CsrMatrix {
    CsrMatrix::from_triplets(rows, cols, &[], &[], &[]).unwrap_or_else(|_| CsrMatrix::identity(0))
}

#[must_use]
pub fn floats(values: &[f64]) -> Value {
    json!(values)
}
