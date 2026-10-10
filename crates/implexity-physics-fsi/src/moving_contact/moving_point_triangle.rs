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

pub struct InteriorGeometry<S> {
    pub gap: S,
    pub normal: [S; 3],
    pub barycentric: [S; 3],
}
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
pub fn interior_geometry<S: Scalar>(
    positions: [[S; 3]; 4],
    minimum_barycentric: f64,
    minimum_area_ratio: f64,
) -> Result<InteriorGeometry<S>, &'static str> {
    if positions.iter().flatten().any(|x| !x.value().is_finite()) {
        return Err("finite current coordinates required");
    }
    if !minimum_barycentric.is_finite()
        || minimum_barycentric <= 0.0
        || minimum_barycentric >= 1.0 / 3.0
        || !minimum_area_ratio.is_finite()
        || minimum_area_ratio <= 0.0
        || minimum_area_ratio >= 1.0
    {
        return Err("invalid declared feature eligibility");
    }
    let [p, a, b, c] = positions;
    let u = sub(b, a);
    let v = sub(c, a);
    let w = sub(p, a);
    let norm = |edge: &[S; 3]| {
        edge.iter()
            .fold(0.0_f64, |length, x| length.hypot(x.value()))
    };
    let scale = norm(&u).max(norm(&v));
    if !scale.is_finite() || scale <= 0.0 {
        return Err("degenerate or unrepresentable edges");
    }
    let factor = S::from_f64(scale);
    let u = u.map(|x| x / factor);
    let v = v.map(|x| x / factor);
    let ws = w.map(|x| x / factor);
    let n = cross(u, v);
    let n2 = dot(n, n);
    if !n2.value().is_finite() || n2.value() <= minimum_area_ratio * minimum_area_ratio {
        return Err("triangle below declared area eligibility");
    }
    let length = n2.sqrt();
    let normal = n.map(|x| x / length);
    let beta = dot(cross(ws, v), n) / n2;
    let gamma = dot(cross(u, ws), n) / n2;
    let barycentric = [S::from_f64(1.0) - beta - gamma, beta, gamma];
    if barycentric
        .iter()
        .any(|x| !x.value().is_finite() || x.value() <= minimum_barycentric)
    {
        return Err("projection outside strict interior feature");
    }
    let gap = dot(w, normal);
    if !gap.value().is_finite() {
        return Err("unrepresentable signed gap");
    }
    Ok(InteriorGeometry {
        gap,
        normal,
        barycentric,
    })
}
