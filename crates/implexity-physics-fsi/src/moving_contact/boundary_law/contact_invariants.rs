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
pub(super) fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] { std::array::from_fn(|i| a[i] - b[i]) }
pub(super) fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S { a[0]*b[0] + a[1]*b[1] + a[2]*b[2] }
pub(super) fn invariants<S: Scalar>(p: [[S; 3]; 4]) -> [S; 6] {
    let u = sub(p[2],p[1]); let v = sub(p[3],p[1]); let w = sub(p[0],p[1]);
    [dot(u,u),dot(v,v),dot(w,w),dot(u,v),dot(u,w),dot(v,w)]
}
pub(super) fn invariant_gradient<S: Scalar>(old: [S;6], new: [S;6], h0:S, h1:S) -> [S;6] {
    let c=S::from_f64; let mid: [S;6]=std::array::from_fn(|i|(old[i]+new[i])*c(0.5));
    let d: [S;6]=std::array::from_fn(|i|new[i]-old[i]);
    let d0=old[0]*old[1]-old[3]*old[3]; let d1=new[0]*new[1]-new[3]*new[3];
    let dm=(d0+d1)*c(0.5); let hm=(h0+h1)*c(0.5);
    let gd=[mid[1],mid[0],c(0.),c(-2.)*mid[3],c(0.),c(0.)];
    let mut gn=gd.map(|x|x*mid[2]); gn[2]=gn[2]+dm;
    gn[1]=gn[1]-(old[4]*old[4]+new[4]*new[4])*c(0.5);
    gn[4]=gn[4]-c(2.)*mid[1]*mid[4];
    gn[0]=gn[0]-(old[5]*old[5]+new[5]*new[5])*c(0.5);
    gn[5]=gn[5]-c(2.)*mid[0]*mid[5];
    gn[3]=gn[3]+c(2.)*(mid[4]*mid[5]+d[4]*d[5]/c(12.));
    gn[4]=gn[4]+c(2.)*(mid[3]*mid[5]+d[3]*d[5]/c(12.));
    gn[5]=gn[5]+c(2.)*(mid[3]*mid[4]+d[3]*d[4]/c(12.));
    std::array::from_fn(|i|(gn[i]-hm*gd[i])/dm)
}
pub(super) fn coordinate_gradient<S:Scalar>(p:[[S;3];4],g:[S;6])->[S;12] {
    let c=S::from_f64;let u=sub(p[2],p[1]);let v=sub(p[3],p[1]);let w=sub(p[0],p[1]);
    let fu: [S;3]=std::array::from_fn(|a|c(2.)*g[0]*u[a]+g[3]*v[a]+g[4]*w[a]);
    let fv: [S;3]=std::array::from_fn(|a|c(2.)*g[1]*v[a]+g[3]*u[a]+g[5]*w[a]);
    let fw: [S;3]=std::array::from_fn(|a|c(2.)*g[2]*w[a]+g[4]*u[a]+g[5]*v[a]);
    std::array::from_fn(|i|match i/3 {0=>fw[i%3],1=>-(fu[i%3]+fv[i%3]+fw[i%3]),2=>fu[i%3],_=>fv[i%3]})
}
