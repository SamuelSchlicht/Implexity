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

use crate::moving_contact::{
    mapped_pair_law::NodeBinding, native_surface_binding::NativeNodalBinding,
    surface_map::SurfaceFeature,
};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;

fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn add<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn scale<S: Scalar>(a: [S; 3], b: S) -> [S; 3] {
    std::array::from_fn(|i| a[i] * b)
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).fold(S::zero(), |a, b| a + b)
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
fn centroid<S: Scalar>(p: [[S; 3]; 3]) -> [S; 3] {
    scale(add(add(p[0], p[1]), p[2]), S::from_f64(1. / 3.))
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceGapCell {
    pub features: [[SurfaceFeature; 3]; 2],
    pub minimum_area_m2: f64,
    pub minimum_wedge_jacobian_m3: f64,
    pub maximum_tangent_offset_ratio: f64,
}

#[derive(Clone, Debug)]
pub struct GapCellGeometry<S> {
    pub vertices_m: [[S; 3]; 6],
    pub area_vector_m2: [S; 3],
    pub area_m2: S,
    pub normal: [S; 3],
    pub tangent: [[S; 3]; 2],
    pub local_separation_m: [S; 3],
    pub volume_m3: S,
    pub hydraulic_gap_m: S,
}

#[derive(Clone, Debug)]
pub struct GapPressureLoads<S> {
    pub cover_n: [[S; 3]; 6],
    pub cap_n: [[[S; 3]; 6]; 3],
    pub volume_gradient_load_n: [[S; 3]; 6],
}

#[derive(Clone, Debug)]
pub struct GapDissipativeLoads<S> {
    pub force_n: [[S; 3]; 6],
    pub local_rate_m_s: [S; 3],
    pub normal_dissipation_w: S,
    pub tangential_dissipation_w: S,
}

#[derive(Clone)]
pub struct SurfaceGapKinematics {
    nodes: [Vec<NodeBinding>; 2],
    phase: CsrMatrix,
    state_size: usize,
    force_size: usize,
    cells: Vec<SurfaceGapCell>,
}

fn feature_nodes(feature: &SurfaceFeature) -> Vec<usize> {
    match feature {
        SurfaceFeature::Vertex(i) => vec![*i],
        SurfaceFeature::FixedTetrahedron { nodes, .. } => nodes.to_vec(),
        SurfaceFeature::DensityEdge { nodes, .. } => nodes.to_vec(),
    }
}
fn weights<S: Scalar>(
    feature: &SurfaceFeature,
    density: &std::collections::BTreeMap<usize, S>,
) -> CaeResult<Vec<(usize, S)>> {
    let nodes = feature_nodes(feature);
    let values = nodes
        .iter()
        .map(|i| {
            density
                .get(i)
                .map(Scalar::value)
                .ok_or_else(|| fail("surface-gap feature density index"))
        })
        .collect::<CaeResult<Vec<_>>>()?;
    if nodes
        .iter()
        .enumerate()
        .any(|(i, n)| nodes[..i].contains(n))
    {
        return Err(fail("repeated surface-gap feature node"));
    }
    let local = match feature {
        SurfaceFeature::Vertex(_) => SurfaceFeature::Vertex(0),
        SurfaceFeature::FixedTetrahedron {
            weights,
            weight_tolerance,
            ..
        } => SurfaceFeature::FixedTetrahedron {
            nodes: [0, 1, 2, 3],
            weights: *weights,
            weight_tolerance: *weight_tolerance,
        },
        SurfaceFeature::DensityEdge {
            iso,
            contrast_min,
            interior_margin,
            ..
        } => SurfaceFeature::DensityEdge {
            nodes: [0, 1],
            iso: *iso,
            contrast_min: *contrast_min,
            interior_margin: *interior_margin,
        },
    };
    let _ = local.map(&values)?;
    Ok(match *feature {
        SurfaceFeature::Vertex(i) => vec![(i, S::from_f64(1.))],
        SurfaceFeature::FixedTetrahedron { nodes, weights, .. } => nodes
            .into_iter()
            .zip(weights.into_iter().map(S::from_f64))
            .collect(),
        SurfaceFeature::DensityEdge {
            nodes: [a, b], iso, ..
        } => {
            let t = (S::from_f64(iso) - density[&a]) / (density[&b] - density[&a]);
            vec![(a, S::from_f64(1.) - t), (b, t)]
        }
    })
}

impl SurfaceGapKinematics {
    pub fn from_native(
        binding: &NativeNodalBinding,
        cells: Vec<SurfaceGapCell>,
    ) -> CaeResult<Self> {
        Self::new(
            binding.nodes().clone(),
            binding.phase().clone(),
            binding.state_size(),
            binding.flux_size(),
            cells,
        )
    }
    pub fn new(
        nodes: [Vec<NodeBinding>; 2],
        phase: CsrMatrix,
        state_size: usize,
        force_size: usize,
        cells: Vec<SurfaceGapCell>,
    ) -> CaeResult<Self> {
        if cells.is_empty()
            || phase.nrows() != nodes[0].len() + nodes[1].len()
            || state_size == 0
            || force_size == 0
            || nodes.iter().flatten().any(|n| {
                n.reference_m.iter().any(|v| !v.is_finite())
                    || n.inverse_state_scale
                        .iter()
                        .any(|v| !v.is_finite() || *v <= 0.)
                    || n.state.iter().any(|i| *i >= state_size)
                    || n.force_flux.iter().any(|i| *i >= force_size)
            })
            || cells.iter().any(|c| {
                ![
                    c.minimum_area_m2,
                    c.minimum_wedge_jacobian_m3,
                    c.maximum_tangent_offset_ratio,
                ]
                .iter()
                .all(|v| v.is_finite() && *v > 0.)
            })
        {
            return Err(fail("invalid native surface-gap layout"));
        }
        let mut state_bindings = std::collections::BTreeMap::new();
        let mut force_bindings = std::collections::BTreeMap::new();
        for node in nodes.iter().flatten() {
            for axis in 0..3 {
                let association = (
                    node.force_flux[axis],
                    node.inverse_state_scale[axis].to_bits(),
                );
                if state_bindings
                    .insert(node.state[axis], association)
                    .is_some_and(|old| old != association)
                    || force_bindings
                        .insert(node.force_flux[axis], node.state[axis])
                        .is_some_and(|old| old != node.state[axis])
                {
                    return Err(fail(
                        "inconsistent native surface-gap degree-of-freedom binding",
                    ));
                }
            }
        }
        Ok(Self {
            nodes,
            phase,
            state_size,
            force_size,
            cells,
        })
    }
    pub fn as_trace_kinematics(&self) -> CaeResult<Self> {
        let mut nodes = self.nodes.clone();
        for node in nodes.iter_mut().flatten() {
            node.state = node.force_flux;
            node.inverse_state_scale = [1.; 3];
        }
        Self::new(
            nodes,
            self.phase.clone(),
            self.force_size,
            self.force_size,
            self.cells.clone(),
        )
    }
    pub fn compact(&self) -> CaeResult<(Self, Vec<usize>, Vec<usize>)> {
        let trace_map = self.active_trace_indices()?;
        let design_map = self.active_design_indices()?;
        let trace_inverse: std::collections::BTreeMap<usize, usize> =
            trace_map.iter().enumerate().map(|(i, &v)| (v, i)).collect();
        let design_inverse: std::collections::BTreeMap<usize, usize> = design_map
            .iter()
            .enumerate()
            .map(|(i, &v)| (v, i))
            .collect();
        let mut selected: [std::collections::BTreeSet<usize>; 2] =
            std::array::from_fn(|_| std::collections::BTreeSet::new());
        for c in &self.cells {
            for body in 0..2 {
                for f in &c.features[body] {
                    selected[body].extend(feature_nodes(f));
                }
            }
        }
        let node_map: [std::collections::BTreeMap<usize, usize>; 2] = std::array::from_fn(|body| {
            selected[body]
                .iter()
                .enumerate()
                .map(|(i, &v)| (v, i))
                .collect()
        });
        let mut nodes: [Vec<NodeBinding>; 2] = std::array::from_fn(|_| Vec::new());
        let (mut rows, mut columns, mut values) = (Vec::new(), Vec::new(), Vec::new());
        for body in 0..2 {
            for (&old, &new) in &node_map[body] {
                let source = self.nodes[body]
                    .get(old)
                    .ok_or_else(|| fail("compact gap native node index"))?;
                let flux = source.force_flux.map(|i| trace_inverse[&i]);
                nodes[body].push(NodeBinding {
                    reference_m: source.reference_m,
                    state: flux,
                    inverse_state_scale: [1.; 3],
                    force_flux: flux,
                });
                let old_row = old + if body == 0 { 0 } else { self.nodes[0].len() };
                let new_row = new + if body == 0 { 0 } else { node_map[0].len() };
                let (col, val) = self.phase.row(old_row);
                for (&c, &v) in col.iter().zip(val) {
                    if let Some(&mapped) = design_inverse.get(&c) {
                        if v != 0. {
                            rows.push(new_row);
                            columns.push(mapped);
                            values.push(v);
                        }
                    }
                }
            }
        }
        let mut cells = self.cells.clone();
        for c in &mut cells {
            for body in 0..2 {
                for f in &mut c.features[body] {
                    *f = match f.clone() {
                        SurfaceFeature::Vertex(i) => SurfaceFeature::Vertex(node_map[body][&i]),
                        SurfaceFeature::FixedTetrahedron {
                            nodes,
                            weights,
                            weight_tolerance,
                        } => SurfaceFeature::FixedTetrahedron {
                            nodes: nodes.map(|i| node_map[body][&i]),
                            weights,
                            weight_tolerance,
                        },
                        SurfaceFeature::DensityEdge {
                            nodes,
                            iso,
                            contrast_min,
                            interior_margin,
                        } => SurfaceFeature::DensityEdge {
                            nodes: nodes.map(|i| node_map[body][&i]),
                            iso,
                            contrast_min,
                            interior_margin,
                        },
                    };
                }
            }
        }
        let phase = CsrMatrix::from_triplets(
            nodes[0].len() + nodes[1].len(),
            design_map.len(),
            &rows,
            &columns,
            &values,
        )
        .map_err(|e| fail(&e.to_string()))?;
        let compact = Self::new(nodes, phase, trace_map.len(), trace_map.len(), cells)?;
        Ok((compact, trace_map, design_map))
    }
    pub fn active_trace_indices(&self) -> CaeResult<Vec<usize>> {
        let mut indices = std::collections::BTreeSet::new();
        for cell in &self.cells {
            for body in 0..2 {
                for feature in &cell.features[body] {
                    let nodes = match feature {
                        SurfaceFeature::Vertex(i) => vec![*i],
                        SurfaceFeature::FixedTetrahedron { nodes, .. } => nodes.to_vec(),
                        SurfaceFeature::DensityEdge { nodes, .. } => nodes.to_vec(),
                    };
                    for i in nodes {
                        let node = self.nodes[body]
                            .get(i)
                            .ok_or_else(|| fail("surface-gap active feature node"))?;
                        indices.extend(node.force_flux);
                    }
                }
            }
        }
        Ok(indices.into_iter().collect())
    }
    pub fn active_design_indices(&self) -> CaeResult<Vec<usize>> {
        self.active_design_indices_for_cells(&(0..self.cells.len()).collect::<Vec<_>>())
    }
    pub(crate) fn active_design_indices_for_cells(&self, cells: &[usize]) -> CaeResult<Vec<usize>> {
        let mut indices = std::collections::BTreeSet::new();
        for &index in cells {
            let cell = self
                .cells
                .get(index)
                .ok_or_else(|| fail("surface-gap active cell index"))?;
            for body in 0..2 {
                for feature in &cell.features[body] {
                    if let SurfaceFeature::DensityEdge { nodes, .. } = feature {
                        for &i in nodes {
                            if i >= self.nodes[body].len() {
                                return Err(fail("surface-gap active density node"));
                            }
                            let row = i + if body == 0 { 0 } else { self.nodes[0].len() };
                            let (columns, values) = self.phase.row(row);
                            for (&column, &value) in columns.iter().zip(values) {
                                if value != 0. {
                                    indices.insert(column);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(indices.into_iter().collect())
    }
    pub fn vertex_velocities<S: Scalar>(
        &self,
        current_trace: &[S],
        previous_trace: &[S],
        design: &[S],
        time_step_s: S,
    ) -> CaeResult<Vec<[[S; 3]; 6]>> {
        if current_trace.len() != self.force_size
            || previous_trace.len() != self.force_size
            || current_trace
                .iter()
                .chain(previous_trace)
                .any(|v| !v.value().is_finite())
            || !time_step_s.value().is_finite()
            || time_step_s.value() <= 0.
        {
            return Err(fail("invalid surface-gap trace velocity fields"));
        }
        let density = self.densities(design)?;
        self.cells
            .iter()
            .map(|cell| {
                let mut velocity = [[S::zero(); 3]; 6];
                for body in 0..2 {
                    for vertex in 0..3 {
                        for (node, weight) in weights(&cell.features[body][vertex], &density[body])?
                        {
                            for axis in 0..3 {
                                let dof = self.nodes[body][node].force_flux[axis];
                                velocity[body * 3 + vertex][axis] += weight
                                    * (current_trace[dof] - previous_trace[dof])
                                    / time_step_s;
                            }
                        }
                    }
                }
                if velocity.iter().flatten().any(|v| !v.value().is_finite()) {
                    return Err(fail("surface-gap vertex velocity overflow"));
                }
                Ok(velocity)
            })
            .collect()
    }
    pub(crate) fn binding_identity(&self) -> String {
        implexity_core::json::canonical_sha256(
            &serde_json::json!({"nodes":self.nodes.iter().map(|b|b.iter().map(|n|serde_json::json!({"reference_m":n.reference_m,"state":n.state,"inverse_state_scale":n.inverse_state_scale,"force_flux":n.force_flux})).collect::<Vec<_>>()).collect::<Vec<_>>(),"phase":(0..self.phase.nrows()).map(|r|{let(c,v)=self.phase.row(r);serde_json::json!([c,v])}).collect::<Vec<_>>(),"state_size":self.state_size,"force_size":self.force_size,"cells":self.cells}),
        )
    }
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }
    pub(crate) fn chart_features(&self, chart: usize) -> CaeResult<[[SurfaceFeature; 3]; 2]> {
        self.cells
            .get(chart)
            .map(|cell| cell.features.clone())
            .ok_or_else(|| fail("surface-gap chart feature index"))
    }
    pub fn state_size(&self) -> usize {
        self.state_size
    }
    pub fn force_size(&self) -> usize {
        self.force_size
    }
    pub fn design_size(&self) -> usize {
        self.phase.ncols()
    }
    fn densities<S: Scalar>(
        &self,
        design: &[S],
    ) -> CaeResult<[std::collections::BTreeMap<usize, S>; 2]> {
        if design.len() != self.phase.ncols() || design.iter().any(|v| !v.value().is_finite()) {
            return Err(fail("surface-gap design shape or value"));
        }
        let mut density: [std::collections::BTreeMap<usize, S>; 2] =
            std::array::from_fn(|_| std::collections::BTreeMap::new());
        for cell in &self.cells {
            for body in 0..2 {
                for feature in &cell.features[body] {
                    for node in feature_nodes(feature) {
                        if node >= self.nodes[body].len() {
                            return Err(fail("surface-gap feature native node index"));
                        }
                        if density[body].contains_key(&node) {
                            continue;
                        }
                        let row = node + if body == 0 { 0 } else { self.nodes[0].len() };
                        let (columns, values) = self.phase.row(row);
                        let mut value = S::zero();
                        for (&column, &weight) in columns.iter().zip(values) {
                            if !weight.is_finite() {
                                return Err(fail("surface-gap phase coefficient"));
                            }
                            value += design[column] * weight;
                        }
                        density[body].insert(node, value);
                    }
                }
            }
        }
        Ok(density)
    }
    pub fn geometry<S: Scalar>(
        &self,
        state: &[S],
        design: &[S],
    ) -> CaeResult<Vec<GapCellGeometry<S>>> {
        self.mapped_geometry(state, design, |v, c| {
            cell_geometry(
                v,
                c.minimum_area_m2,
                c.minimum_wedge_jacobian_m3,
                c.maximum_tangent_offset_ratio,
            )
        })
    }
    pub fn geometry_from_trace<S: Scalar>(
        &self,
        trace: &[S],
        design: &[S],
    ) -> CaeResult<Vec<GapCellGeometry<S>>> {
        self.mapped_geometry_from_trace(trace, design, |v, c| {
            cell_geometry(
                v,
                c.minimum_area_m2,
                c.minimum_wedge_jacobian_m3,
                c.maximum_tangent_offset_ratio,
            )
        })
    }
    pub fn mapped_geometry<S: Scalar, T>(
        &self,
        state: &[S],
        design: &[S],
        map: impl Fn([[S; 3]; 6], &SurfaceGapCell) -> CaeResult<T>,
    ) -> CaeResult<Vec<T>> {
        if state.len() < self.state_size || state.iter().any(|v| !v.value().is_finite()) {
            return Err(fail("surface-gap state shape or value"));
        }
        let density = self.densities(design)?;
        self.cells
            .iter()
            .map(|cell| {
                let mut vertices = [[S::zero(); 3]; 6];
                for body in 0..2 {
                    for vertex in 0..3 {
                        let w = weights(&cell.features[body][vertex], &density[body])?;
                        for (node, weight) in w {
                            for axis in 0..3 {
                                let binding = &self.nodes[body][node];
                                vertices[body * 3 + vertex][axis] +=
                                    (S::from_f64(binding.reference_m[axis])
                                        + state[binding.state[axis]]
                                            * binding.inverse_state_scale[axis])
                                        * weight;
                            }
                        }
                    }
                }
                map(vertices, cell)
            })
            .collect()
    }
    pub fn mapped_geometry_from_trace<S: Scalar, T>(
        &self,
        trace: &[S],
        design: &[S],
        map: impl Fn([[S; 3]; 6], &SurfaceGapCell) -> CaeResult<T>,
    ) -> CaeResult<Vec<T>> {
        if trace.len() != self.force_size || trace.iter().any(|v| !v.value().is_finite()) {
            return Err(fail("surface-gap mechanical trace shape or value"));
        }
        let mut state = vec![S::zero(); self.state_size];
        for node in self.nodes.iter().flatten() {
            for axis in 0..3 {
                state[node.state[axis]] =
                    trace[node.force_flux[axis]] / node.inverse_state_scale[axis];
            }
        }
        self.mapped_geometry(&state, design, map)
    }

    pub fn scatter_cover<S: Scalar>(
        &self,
        forces: &[[[S; 3]; 6]],
        design: &[S],
    ) -> CaeResult<Vec<S>> {
        if forces.len() != self.cells.len()
            || forces
                .iter()
                .flatten()
                .flatten()
                .any(|v| !v.value().is_finite())
        {
            return Err(fail("surface-gap cover force shape or value"));
        }
        let density = self.densities(design)?;
        let mut out = vec![S::zero(); self.force_size];
        for (cell, force) in self.cells.iter().zip(forces) {
            for body in 0..2 {
                for vertex in 0..3 {
                    for (node, weight) in weights(&cell.features[body][vertex], &density[body])? {
                        for axis in 0..3 {
                            out[self.nodes[body][node].force_flux[axis]] +=
                                weight * force[body * 3 + vertex][axis];
                        }
                    }
                }
            }
        }
        if out.iter().any(|v| !v.value().is_finite()) {
            return Err(fail("surface-gap force scatter overflow"));
        }
        Ok(out)
    }
}

pub fn cell_geometry<S: Scalar>(
    vertices: [[S; 3]; 6],
    minimum_area: f64,
    minimum_jacobian: f64,
    maximum_tangent_ratio: f64,
) -> CaeResult<GapCellGeometry<S>> {
    if vertices.iter().flatten().any(|v| !v.value().is_finite())
        || ![minimum_area, minimum_jacobian, maximum_tangent_ratio]
            .iter()
            .all(|v| v.is_finite() && *v > 0.)
    {
        return Err(fail("invalid paired wedge geometry"));
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
    let jac0 = cross(el[0], el[1]);
    let jac1 = add(cross(de[0], el[1]), cross(el[0], de[1]));
    let jac2 = cross(de[0], de[1]);
    for vertex in 0..3 {
        let difference = sub(upper[vertex], lower[vertex]);
        let a = dot(jac0, difference).value();
        let b = dot(jac1, difference).value();
        let c = dot(jac2, difference).value();
        let mut minimum = a.min(a + b + c);
        if c > 0. {
            let t = -b / (2. * c);
            if t > 0. && t < 1. {
                minimum = minimum.min(a + b * t + c * t * t);
            }
        }
        if !minimum.is_finite() || minimum <= minimum_jacobian {
            return Err(fail("paired wedge is inverted or degenerate"));
        }
    }
    if !area.value().is_finite() || area.value() <= minimum_area || norm(mid[0]).value() <= 0. {
        return Err(fail("paired wedge surface area is degenerate"));
    }
    let n = scale(c, S::from_f64(1.) / area);
    let t1 = scale(mid[0], S::from_f64(1.) / norm(mid[0]));
    let t2 = cross(n, t1);
    let d = sub(centroid(upper), centroid(lower));
    let local = [dot(d, t1), dot(d, t2), dot(d, n)];
    let volume = dot(d, add(c, scale(jac2, S::from_f64(1. / 24.))));
    let tangent = local[0].value().hypot(local[1].value());
    if local[2].value() <= 0.
        || tangent / local[2].value() > maximum_tangent_ratio
        || !volume.value().is_finite()
        || volume.value() <= 0.
    {
        return Err(fail("paired wedge lies outside its gap chart"));
    }
    Ok(GapCellGeometry {
        vertices_m: vertices,
        area_vector_m2: c,
        area_m2: area,
        normal: n,
        tangent: [t1, t2],
        local_separation_m: local,
        volume_m3: volume,
        hydraulic_gap_m: volume / area,
    })
}

pub fn pressure_loads<S: Scalar>(
    geometry: &GapCellGeometry<S>,
    pressure: S,
) -> CaeResult<GapPressureLoads<S>> {
    if !pressure.value().is_finite()
        || geometry
            .vertices_m
            .iter()
            .flatten()
            .any(|v| !v.value().is_finite())
    {
        return Err(fail("nonfinite gap pressure"));
    }
    let v = geometry.vertices_m;
    let cl = scale(cross(sub(v[1], v[0]), sub(v[2], v[0])), S::from_f64(0.5));
    let cu = scale(cross(sub(v[4], v[3]), sub(v[5], v[3])), S::from_f64(0.5));
    let mut cover = [[S::zero(); 3]; 6];
    for i in 0..3 {
        cover[i] = scale(cl, -pressure / 3.);
        cover[i + 3] = scale(cu, pressure / 3.);
    }
    let mut caps = [[[S::zero(); 3]; 6]; 3];
    let quadrature = [0.5 - 0.5 / 3f64.sqrt(), 0.5 + 0.5 / 3f64.sqrt()];
    for edge in 0..3 {
        let i = edge;
        let j = (edge + 1) % 3;
        for &s in &quadrature {
            for &t in &quadrature {
                let tangent_s = add(
                    scale(sub(v[j], v[i]), S::from_f64(1. - t)),
                    scale(sub(v[j + 3], v[i + 3]), S::from_f64(t)),
                );
                let tangent_t = add(
                    scale(sub(v[i + 3], v[i]), S::from_f64(1. - s)),
                    scale(sub(v[j + 3], v[j]), S::from_f64(s)),
                );
                let force = scale(cross(tangent_s, tangent_t), pressure * 0.25);
                for (node, w) in [
                    (i, (1. - s) * (1. - t)),
                    (j, s * (1. - t)),
                    (j + 3, s * t),
                    (i + 3, (1. - s) * t),
                ] {
                    caps[edge][node] = add(caps[edge][node], scale(force, S::from_f64(w)));
                }
            }
        }
    }
    let total =
        std::array::from_fn(|i| add(cover[i], add(add(caps[0][i], caps[1][i]), caps[2][i])));
    Ok(GapPressureLoads {
        cover_n: cover,
        cap_n: caps,
        volume_gradient_load_n: total,
    })
}

pub fn local_separation_gradients<S: Scalar>(g: &GapCellGeometry<S>) -> [[[S; 3]; 6]; 3] {
    let v = g.vertices_m;
    let mid0 = scale(add(v[0], v[3]), S::from_f64(0.5));
    let mid1 = scale(add(v[1], v[4]), S::from_f64(0.5));
    let mid2 = scale(add(v[2], v[5]), S::from_f64(0.5));
    let e1 = sub(mid1, mid0);
    let e2 = sub(mid2, mid0);
    let d = sub(centroid([v[3], v[4], v[5]]), centroid([v[0], v[1], v[2]]));
    std::array::from_fn(|axis| {
        let basis = if axis == 2 { g.normal } else { g.tangent[axis] };
        let gn = match axis {
            2 => d,
            1 => cross(g.tangent[0], d),
            _ => [S::zero(); 3],
        };
        let gt1 = match axis {
            0 => d,
            1 => cross(d, g.normal),
            _ => [S::zero(); 3],
        };
        let gc = scale(
            sub(gn, scale(g.normal, dot(gn, g.normal))),
            S::from_f64(1.) / g.area_m2,
        );
        let direct = scale(
            sub(gt1, scale(g.tangent[0], dot(gt1, g.tangent[0]))),
            S::from_f64(1.) / norm(e1),
        );
        let ge1 = add(scale(cross(e2, gc), S::from_f64(0.5)), direct);
        let ge2 = scale(cross(gc, e1), S::from_f64(0.5));
        let gm = [scale(add(ge1, ge2), S::from_f64(-1.)), ge1, ge2];
        std::array::from_fn(|node| {
            add(
                scale(
                    basis,
                    S::from_f64(if node < 3 { -1. / 3. } else { 1. / 3. }),
                ),
                scale(gm[node % 3], S::from_f64(0.5)),
            )
        })
    })
}

pub fn area_gradients<S: Scalar>(g: &GapCellGeometry<S>) -> [[S; 3]; 6] {
    let v = g.vertices_m;
    let e1 = scale(add(sub(v[1], v[0]), sub(v[4], v[3])), S::from_f64(0.5));
    let e2 = scale(add(sub(v[2], v[0]), sub(v[5], v[3])), S::from_f64(0.5));
    let ge1 = scale(cross(e2, g.normal), S::from_f64(0.5));
    let ge2 = scale(cross(g.normal, e1), S::from_f64(0.5));
    let gm = [scale(add(ge1, ge2), S::from_f64(-1.)), ge1, ge2];
    std::array::from_fn(|node| scale(gm[node % 3], S::from_f64(0.5)))
}

#[derive(Clone, Debug)]
pub struct GapElasticContact<S> {
    pub energy_j: S,
    pub force_n: [[S; 3]; 6],
}

pub fn elastic_contact_loads<S: Scalar>(
    g: &GapCellGeometry<S>,
    stiffness_pa_per_m: f64,
    asperity_height_m: f64,
) -> CaeResult<GapElasticContact<S>> {
    if ![stiffness_pa_per_m, asperity_height_m]
        .iter()
        .all(|v| v.is_finite() && *v >= 0.)
    {
        return Err(fail("invalid gap contact stiffness"));
    }
    let overlap = if g.local_separation_m[2].value() < asperity_height_m {
        S::from_f64(asperity_height_m) - g.local_separation_m[2]
    } else {
        S::zero()
    };
    let areal_energy = overlap * overlap * (0.5 * stiffness_pa_per_m);
    let energy = areal_energy * g.area_m2;
    let gap_gradient = local_separation_gradients(g)[2];
    let area_gradient = area_gradients(g);
    let pressure = overlap * stiffness_pa_per_m;
    let force = std::array::from_fn(|node| {
        sub(
            scale(gap_gradient[node], pressure * g.area_m2),
            scale(area_gradient[node], areal_energy),
        )
    });
    Ok(GapElasticContact {
        energy_j: energy,
        force_n: force,
    })
}

pub fn dissipative_loads<S: Scalar>(
    g: &GapCellGeometry<S>,
    velocities: [[S; 3]; 6],
    normal_damping_pa_s_m: f64,
    viscosity_pa_s: f64,
    liquid_volume: S,
    asperity_height_m: f64,
) -> CaeResult<GapDissipativeLoads<S>> {
    if velocities.iter().flatten().any(|v| !v.value().is_finite())
        || ![normal_damping_pa_s_m, viscosity_pa_s, asperity_height_m]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.)
        || !liquid_volume.value().is_finite()
        || liquid_volume.value() < 0.
        || liquid_volume.value() > g.volume_m3.value()
    {
        return Err(fail("invalid surface-gap dissipation parameters"));
    }
    let gradient = local_separation_gradients(g);
    let rates = std::array::from_fn(|axis| {
        (0..6)
            .map(|node| dot(gradient[axis][node], velocities[node]))
            .fold(S::zero(), |a, b| a + b)
    });
    let closing = if rates[2].value() < 0. && g.local_separation_m[2].value() < asperity_height_m {
        -rates[2]
    } else {
        S::zero()
    };
    let damping = closing * g.area_m2 * normal_damping_pa_s_m;
    let drag = g.area_m2 * (liquid_volume / g.volume_m3) * viscosity_pa_s / g.hydraulic_gap_m;
    let force = std::array::from_fn(|node| {
        add(
            scale(gradient[2][node], damping),
            add(
                scale(gradient[0][node], -drag * rates[0]),
                scale(gradient[1][node], -drag * rates[1]),
            ),
        )
    });
    Ok(GapDissipativeLoads {
        force_n: force,
        local_rate_m_s: rates,
        normal_dissipation_w: damping * closing,
        tangential_dissipation_w: drag * (rates[0] * rates[0] + rates[1] * rates[1]),
    })
}

impl SurfaceGapKinematics {
    pub(crate) fn validate_native_owner_bindings(
        &self,
        owner: &crate::native_gap_column_owner::NativeGapColumnOwner,
    ) -> CaeResult<()> {
        if self.force_size != owner.trace_size
            || self.phase.nrows() != owner.phase.nrows()
            || self.phase.ncols() != owner.phase.ncols()
            || self
                .nodes
                .iter()
                .zip(&owner.nodes)
                .any(|(a, b)| a.len() != b.len())
        {
            return Err(fail(
                "surface-gap and native capacity owner binding dimensions disagree",
            ));
        }
        for body in 0..2 {
            for (a, b) in self.nodes[body].iter().zip(&owner.nodes[body]) {
                if a.reference_m.map(f64::to_bits) != b.reference_m.map(f64::to_bits)
                    || a.force_flux != b.force_flux
                {
                    return Err(fail(
                        "surface-gap and native capacity owner nodal binding disagrees",
                    ));
                }
            }
        }
        for row in 0..self.phase.nrows() {
            let (ac, av) = self.phase.row(row);
            let (bc, bv) = owner.phase.row(row);
            if ac != bc
                || av
                    .iter()
                    .map(|v| v.to_bits())
                    .ne(bv.iter().map(|v| v.to_bits()))
            {
                return Err(fail(
                    "surface-gap and native capacity owner phase binding disagrees",
                ));
            }
        }
        for cell in &self.cells {
            for body in 0..2 {
                for feature in &cell.features[body] {
                    if feature_nodes(feature)
                        .iter()
                        .any(|&node| node >= owner.nodes[body].len())
                    {
                        return Err(fail("surface-gap feature is outside native body ownership"));
                    }
                    if let SurfaceFeature::DensityEdge { iso, .. } = feature {
                        if iso.to_bits() != owner.iso.to_bits() {
                            return Err(fail(
                                "surface-gap feature and native capacity owner iso disagree",
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl SurfaceGapKinematics {
    pub(crate) fn mapped_chart_from_trace<S: Scalar, T>(
        &self,
        chart: usize,
        trace: &[S],
        design: &[S],
        map: impl FnOnce([[S; 3]; 6], &SurfaceGapCell) -> CaeResult<T>,
    ) -> CaeResult<T> {
        if trace.len() != self.force_size || trace.iter().any(|v| !v.value().is_finite()) {
            return Err(fail("surface-gap mechanical trace shape or value"));
        }
        if design.len() != self.phase.ncols() || design.iter().any(|v| !v.value().is_finite()) {
            return Err(fail("surface-gap design shape or value"));
        }
        let cell = self
            .cells
            .get(chart)
            .ok_or_else(|| fail("surface-gap chart index"))?;
        let mut density: [std::collections::BTreeMap<usize, S>; 2] =
            std::array::from_fn(|_| std::collections::BTreeMap::new());
        for body in 0..2 {
            for feature in &cell.features[body] {
                for node in feature_nodes(feature) {
                    if node >= self.nodes[body].len() {
                        return Err(fail("surface-gap feature native node index"));
                    }
                    if density[body].contains_key(&node) {
                        continue;
                    }
                    let row = node + if body == 0 { 0 } else { self.nodes[0].len() };
                    let (columns, values) = self.phase.row(row);
                    let mut value = S::zero();
                    for (&column, &weight) in columns.iter().zip(values) {
                        if !weight.is_finite() {
                            return Err(fail("surface-gap phase coefficient"));
                        }
                        value += design[column] * weight;
                    }
                    density[body].insert(node, value);
                }
            }
        }
        let mut last_state_binding = std::collections::BTreeMap::new();
        for node in self.nodes.iter().flatten() {
            for axis in 0..3 {
                last_state_binding.insert(
                    node.state[axis],
                    (node.force_flux[axis], node.inverse_state_scale[axis]),
                );
            }
        }
        if last_state_binding
            .values()
            .any(|&(flux, scale)| !(trace[flux].value() / scale).is_finite())
        {
            return Err(fail("surface-gap state shape or value"));
        }
        let mut vertices = [[S::zero(); 3]; 6];
        for body in 0..2 {
            for vertex in 0..3 {
                let w = weights(&cell.features[body][vertex], &density[body])?;
                for (node, weight) in w {
                    for axis in 0..3 {
                        let binding = &self.nodes[body][node];
                        let &(flux, scale) = last_state_binding
                            .get(&binding.state[axis])
                            .ok_or_else(|| fail("surface-gap chart state binding absent"))?;
                        let state = trace[flux] / scale;
                        vertices[body * 3 + vertex][axis] +=
                            (S::from_f64(binding.reference_m[axis])
                                + state * binding.inverse_state_scale[axis])
                                * weight;
                    }
                }
            }
        }
        map(vertices, cell)
    }
}

impl SurfaceGapKinematics {
    pub(crate) fn native_chart_trace_recipe_charts(&self) -> std::collections::BTreeSet<usize> {
        let mut last = std::collections::BTreeMap::new();
        for n in self.nodes.iter().flatten() { for k in 0..3 { last.insert(n.state[k], (n.force_flux[k], n.inverse_state_scale[k])); } }
        self.cells.iter().enumerate().filter_map(|(chart,c)| {
            c.features.iter().enumerate().all(|(body,features)| features.iter().all(|f| feature_nodes(f).iter().all(|&i| {
                let n=&self.nodes[body][i]; (0..3).all(|k| last.get(&n.state[k]).is_some_and(|&(flux,scale)| flux==n.force_flux[k] && scale.is_finite() && scale!=0. && scale==n.inverse_state_scale[k]))
            }))).then_some(chart)
        }).collect()
    }
}
