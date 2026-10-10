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

use crate::gas_quadratic_diffuse_wall::QuadraticWallInteriorAnchor;
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::native_gas_geometry_adapter::NativeGasConvexSupport;
use implexity_core::{CaeError, CaeResult};

pub fn measured_wall_anchor<S: TransientGasScalar>(
    support: &NativeGasConvexSupport<S>,
    component: usize,
    convex_part: usize,
    left_slot: usize,
    face_position_m: [S; 3],
    face_normal: [S; 3],
) -> CaeResult<QuadraticWallInteriorAnchor<S>> {
    if support.component != component || support.convex_part != convex_part {
        return Err(CaeError::contract("native wall convex support ownership differs"));
    }
    let (volume, moment) = crate::native_cut_cell_partition::measure(&support.polygons)?;
    if !volume.finite_first_order() || volume.value() <= 0. {
        return Err(CaeError::contract("native wall convex support volume is not positive"));
    }
    let centroid = moment.map(|v| v / volume);
    let vertices: Vec<_> = support.polygons.iter()
        .filter(|p| !p.support_only)
        .flat_map(|p| p.vertices_m.iter().copied()).collect();
    let planes: Vec<_> = support.polygons.iter()
        .map(|p| (p.outward_normal, p.plane_origin_m, p.plane_offset_m)).collect();
    QuadraticWallInteriorAnchor::from_measured_convex_support(
        left_slot, face_position_m, face_normal, centroid, volume, &vertices, &planes,
    )
}

pub(crate) fn measured_wall_anchor_with_construction<S: TransientGasScalar>(
    support: &NativeGasConvexSupport<S>,
    component: usize,
    convex_part: usize,
    left_slot: usize,
    face_position_m: [S; 3],
    face_normal: [S; 3],
    proof: &crate::native_geometry_construction_error::ConvexSupportConstructionProof,
) -> CaeResult<QuadraticWallInteriorAnchor<S>> {
    if support.component != component || support.convex_part != convex_part {
        return Err(CaeError::contract("native wall convex support ownership differs"));
    }
    let (volume, moment) = crate::native_cut_cell_partition::measure(&support.polygons)?;
    if !volume.finite_first_order() || volume.value() <= 0. {
        return Err(CaeError::contract("native wall convex support volume is not positive"));
    }
    let centroid = moment.map(|v| v / volume);
    let vertices: Vec<_> = support.polygons.iter()
        .filter(|p| !p.support_only)
        .flat_map(|p| p.vertices_m.iter().copied()).collect();
    let planes: Vec<_> = support.polygons.iter()
        .map(|p| (p.outward_normal, p.plane_origin_m, p.plane_offset_m)).collect();
    QuadraticWallInteriorAnchor::from_certified_measured_convex_support(
        left_slot, face_position_m, face_normal, centroid, volume, &vertices, &planes, proof,
    )
}
