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

use super::*;
use crate::native_cut_cell_partition::{native_planes_from_source, CutBoundary, CutPolygon};

impl<B: GeometryTimeBase> NativeSourceFaceWindows<'_, B> {
    pub(crate) fn native_plane_parent_index(
        &self,
    ) -> std::sync::Arc<[BTreeMap<[usize; 3], Vec<usize>>; 2]> {
        self.native_plane_parents
            .get_or_init(|| {
                let owner = self.adapter.owner.capacity_owner();
                let mut parents: [BTreeMap<[usize; 3], Vec<usize>>; 2] =
                    std::array::from_fn(|_| BTreeMap::new());
                for body in 0..2 {
                    for (element, nodes) in owner.elements[body].iter().enumerate() {
                        for face in [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]] {
                            let mut key = face.map(|i| nodes[i]);
                            key.sort();
                            parents[body].entry(key).or_default().push(element);
                        }
                    }
                }
                std::sync::Arc::new(parents)
            })
            .clone()
    }

    pub(crate) fn owns_physical_plane(
        &self,
        window: usize,
        plane: &CutPolygon<f64>,
    ) -> CaeResult<bool> {
        let w = self
            .windows
            .get(window)
            .ok_or_else(|| fail("native plane source window is absent"))?;
        if !matches!(w.kind(), SourceWindowKind::NativePhysical) {
            return Ok(false);
        }
        let CutBoundary::Native {
            body,
            element,
            face,
            physical: true,
        } = plane.boundary
        else {
            return Ok(false);
        };
        let owner = self.adapter.owner.capacity_owner();
        if body >= 2 || element >= owner.elements[body].len() {
            return Ok(false);
        }
        let mut face = face;
        if let Some(ref mut nodes) = face {
            nodes.sort();
        }
        let source_face = match w.facet().owner.as_slice() {
            [1, b, e] if *b == body && *e == element && face.is_none() => None,
            [0, b, a, c, d] if *b == body => {
                let mut nodes = [*a, *c, *d];
                nodes.sort();
                if face != Some(nodes) {
                    return Ok(false);
                }
                let index = self.native_plane_parent_index();
                if !index[body]
                    .get(&nodes)
                    .is_some_and(|parents| parents.as_slice() == [element])
                {
                    return Ok(false);
                }
                Some(nodes)
            }
            _ => return Ok(false),
        };
        let Some(local) = &plane.local_geometry else {
            return Ok(false);
        };
        let anchor = local.anchor();
        let nodes = owner.elements[body][element];
        let positions = nodes
            .map(|node| {
                let binding = &owner.nodes[body][node];
                let reference = owner.node_reference_in_frame(body, node, anchor)?;
                Ok(std::array::from_fn(|k| {
                    self.live_trace[binding.force_flux[k]] + reference[k]
                }))
            })
            .into_iter()
            .collect::<CaeResult<Vec<[GeometryTime<B>; 3]>>>()?;
        let positions: [[GeometryTime<B>; 3]; 4] = positions
            .try_into()
            .map_err(|_| fail("native plane node shape"))?;
        let phase = |node: usize| -> CaeResult<GeometryTime<B>> {
            if node >= owner.nodes[body].len() {
                return Err(fail("native plane phase node is absent"));
            }
            let row = node + if body == 0 { 0 } else { owner.nodes[0].len() };
            let (columns, weights) = owner.phase.row(row);
            Ok(columns
                .iter()
                .zip(weights)
                .map(|(&i, &v)| self.live_design[i] * v)
                .fold(GeometryTime::<B>::zero(), |a, b| a + b))
        };
        let rho = nodes
            .map(phase)
            .into_iter()
            .collect::<CaeResult<Vec<_>>>()?;
        let rho: [GeometryTime<B>; 4] = rho
            .try_into()
            .map_err(|_| fail("native plane phase shape"))?;
        let complete = |v: GeometryTime<B>| {
            v.finite_first_order()
                && v.geometry_first_order_directions()
                    .is_some_and(|d| d.iter().all(|v| v.is_finite()))
        };
        let zero = |v: GeometryTime<B>| {
            complete(v)
                && v.value() == 0.
                && v.geometry_first_order_directions()
                    .is_some_and(|d| d.iter().all(|v| *v == 0.))
        };
        if positions
            .iter()
            .flatten()
            .chain(rho.iter())
            .any(|&v| !complete(v))
        {
            return Err(fail("native plane producer channels are incomplete"));
        }
        for &value in &rho {
            if value.value() == owner.iso && !zero(value - owner.iso) {
                return Err(fail("native plane producer crosses its iso stratum"));
            }
        }
        for feature in &w.facet().vertex_features {
            let ids = match feature.as_slice() {
                [0, b, node] if *b == body => vec![*node],
                [1, b, a, c] if *b == body && a < c => vec![*a, *c],
                _ => return Ok(false),
            };
            if ids
                .iter()
                .any(|n| !nodes.contains(n) || source_face.is_some_and(|f| !f.contains(n)))
            {
                return Ok(false);
            }
            let a = phase(ids[0])?;
            if ids.len() == 1 {
                if source_face.is_none() && !zero(a - owner.iso) {
                    return Ok(false);
                }
                if source_face.is_some() && a.value() < owner.iso {
                    return Ok(false);
                }
            } else {
                let b = phase(ids[1])?;
                if !complete(a)
                    || !complete(b)
                    || !((a.value() < owner.iso && b.value() > owner.iso)
                        || (a.value() > owner.iso && b.value() < owner.iso))
                {
                    return Ok(false);
                }
            }
        }
        let Some(corners) = self.source_corners_in_frame(window, anchor)? else {
            return Ok(false);
        };
        if corners.iter().flatten().any(|&v| !complete(v)) {
            return Err(fail("native plane source channels are incomplete"));
        }
        let exterior = source_face.into_iter().collect();
        let source=owner.element_coefficient_source(body,element,&self.live_trace)?;
        let producer = native_planes_from_source(source,rho,&exterior,owner.iso,anchor)?;
        for expected in producer {
            if expected.boundary != plane.boundary {
                continue;
            }
            if expected
                .normal
                .iter()
                .chain(expected.origin.iter())
                .chain(std::iter::once(&expected.offset))
                .any(|&v| !complete(v))
            {
                return Err(fail("native plane coefficient channels are incomplete"));
            }
            let expected_origin=expected.local_origin.map_or(expected.origin,|(_,origin)|origin);
            if (0..3).any(|k| expected_origin[k].value() != local.plane_origin()[k]) {
                continue;
            }
            for sign in [1., -1.] {
                if (0..3).all(|k| expected.normal[k].value() * sign == plane.outward_normal[k])
                    && expected.offset.value() * sign == plane.plane_offset_m
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}
