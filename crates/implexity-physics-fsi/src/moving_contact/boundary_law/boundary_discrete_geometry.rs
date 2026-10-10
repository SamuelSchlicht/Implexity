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

use implexity_ad::{Dual,Scalar};
#[derive(Clone,Copy,Debug)]pub enum SelectedKind{VertexFace,EdgeEdge{orientation:f64}}
fn selected<S:Scalar>(kind:SelectedKind,p:[[S;3];4])->Result<(S,[S;12],Vec<S>),String>{if p.iter().flatten().any(|v|!v.value().is_finite()){return Err("finite boundary geometry".into());}let(g,gradient,parameters)=match kind{SelectedKind::VertexFace=>{let(g,a,b)=super::boundary_geometry::vf(p,1e-8).map_err(|e|e.to_string())?;(g,a,b.to_vec())},SelectedKind::EdgeEdge{orientation}=>{if orientation!=1.&&orientation!= -1.{return Err("explicit edge orientation".into());}let(g,a,b)=super::boundary_geometry::ee(p,orientation,1e-8).map_err(|e|e.to_string())?;(g,a,b.to_vec())}};Ok((g,std::array::from_fn(|k|gradient[k/3][k%3]),parameters))}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn sum<S: Scalar>(v: impl Iterator<Item = S>) -> S {
    v.fold(S::from_f64(0.), |a, b| a + b)
}
fn law<S: Scalar>(kind:SelectedKind,old: [[S; 3]; 4], new: [[S; 3]; 4]) -> Result<([S; 12], S, S), String> {
    let c = S::from_f64;
    let (gap0,_,_) = selected(kind,old)?;
    let (gap1,_,_) = selected(kind,new)?;
    let mid = std::array::from_fn(|k| std::array::from_fn(|i| (old[k][i] + new[k][i]) * c(0.5)));
    let (_,gradient,_) = selected(kind,mid)?;
    let d: [[S; 3]; 4] = std::array::from_fn(|k| sub(new[k], old[k]));
    if d.iter().flatten().all(|x| x.value() == 0.) {
        return Ok((gradient, gap0, gap1));
    }
    let center: [S; 3] = std::array::from_fn(|i| sum(mid.iter().map(|p| p[i])) / c(4.));
    let mean: [S; 3] = std::array::from_fn(|i| sum(d.iter().map(|p| p[i])) / c(4.));
    let r: [[S; 3]; 4] = mid.map(|p| sub(p, center));
    let v = d.map(|p| sub(p, mean));
    let inertia: [[S; 3]; 3] = std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            sum(r
                .iter()
                .map(|p| (if i == j { dot(*p, *p) } else { c(0.) }) - p[i] * p[j]))
        })
    });
    let moment: [S; 3] =
        std::array::from_fn(|i| sum(r.iter().zip(v).map(|(p, w)| cross(*p, w)[i])));
    let det = dot(inertia[0], cross(inertia[1], inertia[2]));
    if !det.value().is_finite() || det.value() <= 0. {
        return Err("rigid projection degenerate".into());
    }
    let cols = [
        cross(inertia[1], inertia[2]),
        cross(inertia[2], inertia[0]),
        cross(inertia[0], inertia[1]),
    ];
    let omega: [S; 3] = std::array::from_fn(|i| sum((0..3).map(|j| cols[j][i] * moment[j])) / det);
    let projected: [S; 12] = std::array::from_fn(|k| sub(v[k / 3], cross(omega, r[k / 3]))[k % 3]);
    let flat: [S; 12] = std::array::from_fn(|k| d[k / 3][k % 3]);
    let den = sum(projected.iter().map(|x| *x * *x));
    let norm = sum(flat.iter().map(|x| *x * *x));
    if !den.value().is_finite() || den.value() <= 1e-16 * norm.value() {
        return Err("nearly rigid nonzero increment".into());
    }
    let rem = gap1 - gap0 - sum((0..12).map(|i| gradient[i] * flat[i]));
    let result = std::array::from_fn(|i| gradient[i] + rem * projected[i] / den);
    if result.iter().any(|x| !x.value().is_finite()) {
        return Err("discrete gradient overflow".into());
    }
    Ok((result, gap0, gap1))
}
pub struct Geometry {
    pub force: [f64; 12],
    pub current: [[f64; 12]; 12],
    pub previous: [[f64; 12]; 12],
    pub gap0: f64,
    pub gap1: f64,
    pub gap_gradient: [f64; 12],
    pub work_defect: f64,
    pub zero_increment: bool,
}
pub fn evaluate(kind:SelectedKind,old: [[f64; 3]; 4], new: [[f64; 3]; 4]) -> Result<Geometry, String> {
    let a = std::array::from_fn(|k| {
        std::array::from_fn(|i| Dual::<24>::variable(old[k][i], 3 * k + i))
    });
    let b = std::array::from_fn(|k| {
        std::array::from_fn(|i| Dual::<24>::variable(new[k][i], 12 + 3 * k + i))
    });
    let (g, g0, g1) = law(kind,a, b)?;
    let force = g.map(|x| x.re);
    let previous = std::array::from_fn(|i| std::array::from_fn(|j| g[i].eps[j]));
    let current = std::array::from_fn(|i| std::array::from_fn(|j| g[i].eps[12 + j]));
    let gap_gradient = std::array::from_fn(|j| g1.eps[12 + j]);
    if !g0.re.is_finite()||!g1.re.is_finite()||force.iter().any(|v|!v.is_finite()){return Err("boundary force overflow".into());}
    if previous
        .iter()
        .flatten()
        .chain(current.iter().flatten())
        .chain(gap_gradient.iter())
        .any(|x| !x.is_finite())
    {
        return Err("geometry derivative overflow".into());
    }
    let work_defect = (0..12)
        .map(|j| force[j] * (new[j / 3][j % 3] - old[j / 3][j % 3]))
        .sum::<f64>()
        - (g1.re - g0.re);
    Ok(Geometry {
        force,
        current,
        previous,
        gap0: g0.re,
        gap1: g1.re,
        gap_gradient,
        work_defect,
        zero_increment: old == new,
    })
}
pub fn require_ordinary_feature_derivative(kind:SelectedKind,old:[[f64;3];4],new:[[f64;3];4],minimum_parameter:f64)->Result<(),String>{let mid=std::array::from_fn(|i|std::array::from_fn(|j|0.5*(old[i][j]+new[i][j])));for p in [old,mid,new]{let g=match kind{SelectedKind::VertexFace=>super::boundary_geometry::vertex_face(p,[[0.;3];4],1e-8),SelectedKind::EdgeEdge{orientation}=>super::boundary_geometry::edge_edge(p,[[0.;3];4],orientation,1e-8)}.map_err(|e|e.to_string())?;g.require_ordinary_feature_derivative(minimum_parameter).map_err(|e|e.to_string())?;}Ok(())}
