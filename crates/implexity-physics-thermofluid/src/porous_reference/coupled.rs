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


use serde_json::{Map, Value, json};
use std::sync::Arc;
use super::ad::{A, Shape};
use super::{fem, coolant_energy as ce};

fn norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

#[allow(clippy::too_many_arguments)]
pub fn thermal_coupled_residual(
    m: &fem::MeshT,
    shape: [usize; 3],
    k: &[f64],
    g_elem: &[f64],
    q_surface: &[f64],
    t_nodal: &[f64],
    t_elem: &[f64],
    t_fluid: &[f64],
    fp: &ce::FluidParams<'_>,
    setup: &ce::FluidSetup,
    fixed: Option<&(Vec<bool>, Vec<f64>)>,
) -> Map<String, Value> {
    let mut robin = vec![0.0; m.ndof];
    let mut gt = vec![0.0; m.ndof];
    for (e, n) in m.edof.iter().enumerate() {
        for &q in n {
            robin[q] += g_elem[e] / 8.0;
            gt[q] += g_elem[e] * t_fluid[e] / 8.0;
        }
    }
    let mut rhs: Vec<f64> = q_surface.iter().zip(&gt).map(|(a, b)| a + b).collect();
    let kt = m.apply(&k, &robin, t_nodal);
    let mut residual: Vec<f64> = rhs.iter().zip(&kt).map(|(a, b)| a - b).collect();
    if let Some((mask, vals)) = fixed {
        let td: Vec<f64> = (0..m.ndof).map(|i| if mask[i] { vals[i] } else { 0.0 }).collect();
        let ktd = m.apply(&k, &robin, &td);
        for i in 0..m.ndof {
            if mask[i] {
                rhs[i] = 0.0;
                residual[i] = 0.0;
            } else {
                rhs[i] -= ktd[i];
            }
        }
    }
    let nr = norm(&rhs);
    let rel_solid = norm(&residual) / if nr > 0.0 { nr } else { 1.0 };
    let sub = super::ad::Graph::new();
    let s = shape;
    let uf: [A<'_>; 3] = std::array::from_fn(|a| sub.constant(fp.u_face[a].val(), fp.u_face[a].shape()));
    let gv = sub.constant(fp.g_vol.val(), Shape::d3(s));
    let kk = fp.k_axial.map(|k| sub.constant(k.val(), Shape::d3(s)));
    let p2 = ce::FluidParams { u_face: uf, g_vol: gv, k_axial: kk };
    let frhs = ce::load(&p2, setup, sub.constant(t_elem.to_vec(), Shape::d3(s))).val();
    let fapp = ce::apply(&p2, setup, sub.constant(t_fluid.to_vec(), Shape::d3(s))).val();
    let fres: Vec<f64> = frhs.iter().zip(&fapp).map(|(a, b)| a - b).collect();
    let nf = norm(&frhs);
    let mut out = Map::new();
    out.insert("ltne_solid_relative_residual".into(), json!(rel_solid));
    out.insert("ltne_fluid_relative_residual".into(), json!(norm(&fres) / if nf > 0.0 { nf } else { 1.0 }));
    out.insert("ltne_fluid_residual_norm_W".into(), json!(norm(&fres)));
    out.insert("ltne_solid_residual_norm_W".into(), json!(norm(&residual)));
    out
}

pub struct LtneState<'g> {
    pub t_elem: A<'g>,
    pub t_fluid: A<'g>,
    pub t_nodal: A<'g>,
    pub k_elem: A<'g>,
}

#[allow(clippy::too_many_arguments)]
pub fn solve_ltne<'g>(
    mesh: &Arc<fem::MeshT>, edof: &Arc<Vec<[usize; 8]>>, shape: [usize; 3],
    mut t_elem: A<'g>, mut t_fluid: A<'g>, g_elem: A<'g>, q_surface: A<'g>,
    fp: &ce::FluidParams<'g>, setup: &ce::FluidSetup,
    fixed: Option<(&[bool], &[f64])>, n_picard: usize, tol: f64,
    conductivity: impl Fn(A<'g>) -> A<'g>,
) -> LtneState<'g> {
    let g = t_elem.graph();
    let ndof_t = mesh.ndof;
    let robin = fem::lump_element_to_nodes(g_elem, edof, ndof_t);
    let mut t_nodal = g.full(0.0, Shape::d1(ndof_t));
    let mut k_elem = g.full(0.0, Shape::d3(shape));
    for _ in 0..n_picard {
        k_elem = conductivity(t_elem);
        let q_load = q_surface + fem::lump_element_to_nodes(g_elem * t_fluid.flat(), edof, ndof_t);
        t_nodal = fem::solve_temperature(mesh, k_elem.flat(), robin, q_load, tol, fixed);
        t_elem = fem::element_average(t_nodal, edof, shape);
        t_fluid = ce::solve_fluid_temperature(fp, setup, t_elem);
    }
    LtneState { t_elem, t_fluid, t_nodal, k_elem }
}
