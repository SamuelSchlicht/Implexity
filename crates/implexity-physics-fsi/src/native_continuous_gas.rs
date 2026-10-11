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

use crate::gas_geometry_time::GeometryTimeBase;
use crate::gas_quadratic_network::{
    QuadraticGasBoundary, QuadraticGasNetworkFace, QuadraticGasNetworkGeometry,
};
use crate::gas_transient_kinetic::TransientGasBoundary;
use crate::gas_transient_kinetic::TransientGasFace;
use crate::native_gas_geometry_adapter::{
    NativeGasGeometryAssembly, NativeGasGeometryEndpoint, NativeGasGeometryPath,
};
use implexity_core::{CaeError, CaeResult};

pub fn network_geometry<S: GeometryTimeBase>(
    assembly: &NativeGasGeometryAssembly<S>,
    averaging_windows_s: &[S],
) -> CaeResult<QuadraticGasNetworkGeometry<S>> {
    geometry_parts(
        assembly.step_s,
        &assembly.faces,
        &assembly.left_centroid_to_face_m,
        &assembly.right_centroid_to_face_m,
        &assembly.old_geometry,
        &assembly.current_geometry,
        averaging_windows_s,
    )
}

pub fn network_geometry_path<S: GeometryTimeBase>(
    path: &NativeGasGeometryPath<S>,
    averaging_windows_s: &[S],
) -> CaeResult<QuadraticGasNetworkGeometry<S>> {
    let mut geometry = geometry_parts(
        path.step_s,
        &path.faces,
        &path.left_centroid_to_face_m,
        &path.right_centroid_to_face_m,
        &path.old_geometry,
        &path.current_geometry,
        averaging_windows_s,
    )?;
    if path.wall_interior_anchors.len() != geometry.faces.len() {
        return Err(CaeError::contract("native wall anchor face layout differs"));
    }
    for (face, anchor) in geometry.faces.iter_mut().zip(&path.wall_interior_anchors) {
        if let Some(anchor) = anchor {
            if !matches!(face.boundary, Some(QuadraticGasBoundary::DiffuseWall)) || face.area_m2.value() <= 0. {
                return Err(CaeError::contract("native wall anchor requires a positive wall face"));
            }
            anchor.validate_face(face.left, anchor.face_position_m(), face.outward_unit_normal)?;
            face.centroid_m = anchor.face_position_m();
            let frame = path.current_geometry.aggregated.local_centroids_m[face.left].map_or([0.; 3], |p| p.0);
            let center = path.current_geometry.aggregated.centroid_in_frame(face.left, frame)?;
            let position = anchor.face_point_in_frame_m(frame);
            face.local_centroid_m = Some((frame, position));
            face.left_centroid_to_face_m = std::array::from_fn(|k| position[k] - center[k]);
        }
    }
    Ok(geometry)
}

fn geometry_parts<S: GeometryTimeBase>(
    step_s: S,
    source_faces: &[TransientGasFace<S>],
    left_centroid_to_face_m: &[[S; 3]],
    right_centroid_to_face_m: &[Option<[S; 3]>],
    old_geometry: &NativeGasGeometryEndpoint<S>,
    current_geometry: &NativeGasGeometryEndpoint<S>,
    averaging_windows_s: &[S],
) -> CaeResult<QuadraticGasNetworkGeometry<S>> {
    if averaging_windows_s.len() != source_faces.len()
        || left_centroid_to_face_m.len() != source_faces.len()
        || right_centroid_to_face_m.len() != source_faces.len()
    {
        return Err(CaeError::contract(
            "native continuous gas face geometry shape mismatch",
        ));
    }
    let centroids = &current_geometry.aggregated.centroids_m;
    let mut faces = Vec::with_capacity(source_faces.len());
    for (index, face) in source_faces.iter().enumerate() {
        let left_centroid = centroids
            .get(face.left)
            .ok_or_else(|| CaeError::contract("native continuous gas face left slot invalid"))?;
        let (right, boundary) = match face.boundary {
            TransientGasBoundary::Interior { right } => (Some(right), None),
            TransientGasBoundary::Port { port } => {
                (None, Some(QuadraticGasBoundary::Port { port }))
            }
            TransientGasBoundary::DiffuseWall { .. } => {
                (None, Some(QuadraticGasBoundary::DiffuseWall))
            }
        };
        let local_centroid_m = current_geometry.aggregated.local_centroids_m[face.left].map(|(frame, center)| (frame, std::array::from_fn(|k| center[k] + left_centroid_to_face_m[index][k])));
        faces.push(QuadraticGasNetworkFace {
            left: face.left,
            right,
            boundary,
            area_m2: face.area_m2,
            outward_unit_normal: face.normal,
            centroid_m: std::array::from_fn(|axis| {
                local_centroid_m.map_or_else(|| left_centroid[axis] + left_centroid_to_face_m[index][axis], |(a,p)| p[axis] + S::from_f64(a[axis]))
            }),
            local_centroid_m,
            left_centroid_to_face_m: left_centroid_to_face_m[index],
            right_centroid_to_face_m: right_centroid_to_face_m[index],
            right_periodic_image: [0; 3],
            ale_velocity_m_s: face.ale_velocity_m_s,
            material_wall_velocity_m_s: face.wall_velocity_m_s,
            averaging_window_s: averaging_windows_s[index],
        });
    }
    Ok(QuadraticGasNetworkGeometry {
        previous_volume_m3: old_geometry.aggregated.volumes_m3.clone(),
        volume_m3: current_geometry.aggregated.volumes_m3.clone(),
        cell_centroid_m: centroids.clone(),
        cell_local_centroid_m: Some(current_geometry.aggregated.local_centroids_m.clone()),
        cell_central_covariance_m2: current_geometry.aggregated.central_covariance_m2.clone(),
        faces,
        physical_step_s: step_s,
        periodic_basis_m: None,
    })
}
