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

use crate::surface_gap::{GapCellGeometry, GapPressureLoads, SurfaceGapKinematics};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn add<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] + b[i])
}
fn scale<S: Scalar>(a: [S; 3], s: S) -> [S; 3] {
    a.map(|x| x * s)
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm<S: Scalar>(a: [S; 3]) -> S {
    dot(a, a).sqrt()
}
fn centroid<S: Scalar>(a: [[S; 3]; 3]) -> [S; 3] {
    scale(add(add(a[0], a[1]), a[2]), S::from_f64(1. / 3.))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GasGapChartState {
    Closed,
    Open,
}
#[derive(Clone, Debug)]
pub struct GasGapGeometry<S> {
    pub geometry: GapCellGeometry<S>,
    pub state: GasGapChartState,
}
impl<S: Scalar> GasGapGeometry<S> {
    pub fn is_closed(&self) -> bool {
        self.state == GasGapChartState::Closed
    }
    pub fn require_open_derivatives(&self) -> CaeResult<()> {
        if self.is_closed() {
            Err(fail(
                "closed gas chart requires an explicit one-sided opening derivative",
            ))
        } else {
            Ok(())
        }
    }
    pub fn pressure_loads(&self, pressure: S) -> CaeResult<GapPressureLoads<S>> {
        if !pressure.value().is_finite() {
            return Err(fail("nonfinite gas pressure"));
        }
        self.require_open_derivatives()?;
        crate::surface_gap::pressure_loads(&self.geometry, pressure)
    }
}
pub fn cell_geometry<S: Scalar>(
    vertices: [[S; 3]; 6],
    minimum_area_m2: f64,
    maximum_tangent_ratio: f64,
) -> CaeResult<GasGapGeometry<S>> {
    if vertices.iter().flatten().any(|v| !v.value().is_finite())
        || ![minimum_area_m2, maximum_tangent_ratio]
            .iter()
            .all(|v| v.is_finite() && *v > 0.)
    {
        return Err(fail("invalid closed/open gas chart"));
    }
    let lower = [vertices[0], vertices[1], vertices[2]];
    let upper = [vertices[3], vertices[4], vertices[5]];
    let el = [sub(lower[1], lower[0]), sub(lower[2], lower[0])];
    let eu = [sub(upper[1], upper[0]), sub(upper[2], upper[0])];
    let de = [sub(eu[0], el[0]), sub(eu[1], el[1])];
    let mid = [
        scale(add(el[0], eu[0]), S::from_f64(0.5)),
        scale(add(el[1], eu[1]), S::from_f64(0.5)),
    ];
    let c = scale(cross(mid[0], mid[1]), S::from_f64(0.5));
    let area = norm(c);
    if !area.value().is_finite()
        || area.value() <= minimum_area_m2
        || !norm(mid[0]).value().is_finite()
        || norm(mid[0]).value() <= 0.
    {
        return Err(fail("degenerate gas chart area"));
    }
    let jac0 = cross(el[0], el[1]);
    let jac1 = add(cross(de[0], el[1]), cross(el[0], de[1]));
    let jac2 = cross(de[0], de[1]);
    for i in 0..3 {
        let d = sub(upper[i], lower[i]);
        let a = dot(jac0, d).value();
        let b = dot(jac1, d).value();
        let cc = dot(jac2, d).value();
        let mut minimum = a.min(a + b + cc);
        if cc > 0. {
            let t = -b / (2. * cc);
            if t > 0. && t < 1. {
                minimum = minimum.min(a + b * t + cc * t * t);
            }
        }
        if !minimum.is_finite() || minimum < 0. {
            return Err(fail("inverted gas wedge"));
        }
    }
    let n = scale(c, S::one() / area);
    let t1 = scale(mid[0], S::one() / norm(mid[0]));
    let t2 = cross(n, t1);
    let d = sub(centroid(upper), centroid(lower));
    let local = [dot(d, t1), dot(d, t2), dot(d, n)];
    let volume = dot(d, add(c, scale(jac2, S::from_f64(1. / 24.))));
    if !volume.value().is_finite() || volume.value() < 0. {
        return Err(fail("invalid gas wedge volume"));
    }
    let closed =
        (0..3).all(|i| (0..3).all(|a| vertices[i][a].value() == vertices[i + 3][a].value()));
    if !closed
        && (volume.value() <= 0.
            || local[2].value() <= 0.
            || local[0].value().hypot(local[1].value()) / local[2].value() > maximum_tangent_ratio)
    {
        return Err(fail("gas wedge lies outside its native chart"));
    }
    Ok(GasGapGeometry {
        state: if closed {
            GasGapChartState::Closed
        } else {
            GasGapChartState::Open
        },
        geometry: GapCellGeometry {
            vertices_m: vertices,
            area_vector_m2: c,
            area_m2: area,
            normal: n,
            tangent: [t1, t2],
            local_separation_m: local,
            volume_m3: volume,
            hydraulic_gap_m: volume / area,
        },
    })
}
pub fn geometry_from_trace<S: Scalar>(
    kinematics: &SurfaceGapKinematics,
    trace: &[S],
    design: &[S],
) -> CaeResult<Vec<GasGapGeometry<S>>> {
    kinematics.mapped_geometry_from_trace(trace, design, |v, c| {
        cell_geometry(v, c.minimum_area_m2, c.maximum_tangent_offset_ratio)
    })
}
