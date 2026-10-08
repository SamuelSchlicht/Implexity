// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_ad::{Dual, Scalar};
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use serde_json::{Value, json};

use crate::ale_geometry::{Point, TetrahedralAleGeometry, assemble_entity_blocks, err, real_array};

pub const EDGES: [(usize, usize); 6] = [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)];
const FACES: [[usize; 3]; 4] = [[1, 2, 3], [0, 3, 2], [0, 1, 3], [0, 2, 1]];
const CANONICAL: [Point<f64>; 4] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

const THROUGH_NODE: &str = "an interface passes through a node: explicitly rebuild with a nondegenerate topology or use a one-sided shape linearization";
const SIGN_CHANGED: &str = "material sign topology changed at nodes ";

#[must_use]
pub fn is_material_topology_change(error: &CaeError) -> bool {
    let m = error.message();
    m == THROUGH_NODE || m.starts_with(SIGN_CHANGED)
}

fn lu_det(mut a: [[f64; 3]; 3]) -> f64 {
    let mut sign = 1.0;
    for j in 0..3 {
        let mut p = j;
        for i in j + 1..3 {
            if a[i][j].abs() > a[p][j].abs() {
                p = i;
            }
        }
        #[allow(clippy::float_cmp)]
        if a[p][j] == 0.0 {
            return 0.0;
        }
        if p != j {
            a.swap(p, j);
            sign = -sign;
        }
        let r = 1.0 / a[j][j];
        let pivot_row = a[j];
        for row in a.iter_mut().skip(j + 1) {
            let l = row[j] * r;
            for (x, y) in row.iter_mut().zip(pivot_row).skip(j + 1) {
                *x -= l * y;
            }
        }
    }
    sign * a[0][0] * a[1][1] * a[2][2]
}

type Template = (Vec<[usize; 4]>, Vec<[usize; 3]>);

fn cut_template(values: [f64; 4], ranks: [usize; 10]) -> CaeResult<Template> {
    let positive = values.map(|v| v > 0.0);
    let mut positions: Vec<Point<f64>> = CANONICAL.to_vec();
    let mut token = BTreeMap::new();
    for (k, &(i, j)) in EDGES.iter().enumerate() {
        token.insert((i, j), 4 + k);
        let t = if positive[i] == positive[j] { 0.0 } else { values[i] / (values[i] - values[j]) };
        positions.push(std::array::from_fn(|a| (1.0 - t) * CANONICAL[i][a] + t * CANONICAL[j][a]));
    }
    if !positive.iter().any(|p| *p) {
        return Ok((Vec::new(), Vec::new()));
    }
    if positive.iter().all(|p| *p) {
        return Ok((vec![[0, 1, 2, 3]], Vec::new()));
    }
    let key = |i: usize, j: usize| token[&(i.min(j), i.max(j))];
    let mut boundary: Vec<Vec<usize>> = Vec::new();
    for face in FACES {
        let mut clipped = Vec::new();
        for k in 0..3 {
            let (i, j) = (face[k], face[(k + 1) % 3]);
            if positive[i] {
                clipped.push(i);
            }
            if positive[i] != positive[j] {
                clipped.push(key(i, j));
            }
        }
        if clipped.len() >= 3 {
            boundary.push(clipped);
        }
    }
    let pos: Vec<usize> = (0..4).filter(|&k| positive[k]).collect();
    let neg: Vec<usize> = (0..4).filter(|&k| !positive[k]).collect();
    let mut cycle: Vec<usize> = if pos.len() == 2 {
        let (a, b, c, d) = (pos[0], pos[1], neg[0], neg[1]);
        vec![key(a, c), key(a, d), key(b, d), key(b, c)]
    } else {
        EDGES
            .iter()
            .enumerate()
            .filter(|(_, (i, j))| positive[*i] != positive[*j])
            .map(|(k, _)| 4 + k)
            .collect()
    };
    let p = |t: usize| positions[t];
    let (p0, p1, p2) = (p(cycle[0]), p(cycle[1]), p(cycle[2]));
    let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
    let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
    let normal =
        [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
    let gradient = [values[1] - values[0], values[2] - values[0], values[3] - values[0]];
    if normal[0] * gradient[0] + normal[1] * gradient[1] + normal[2] * gradient[2] < 0.0 {
        cycle.reverse();
    }
    let rotate_min = |poly: &[usize]| -> Vec<usize> {
        let first = (0..poly.len()).min_by_key(|&k| ranks[poly[k]]).unwrap_or(0);
        poly[first..].iter().chain(&poly[..first]).copied().collect()
    };
    let cycle = rotate_min(&cycle);
    let interface: Vec<[usize; 3]> =
        (1..cycle.len() - 1).map(|k| [cycle[0], cycle[k], cycle[k + 1]]).collect();
    boundary.push(cycle.iter().rev().copied().collect());
    let anchor = pos.iter().copied().min_by_key(|&k| ranks[k]).unwrap_or(0);
    let mut subtets = Vec::new();
    for polygon in &boundary {
        let polygon = rotate_min(polygon);
        for k in 1..polygon.len() - 1 {
            let triangle = [polygon[0], polygon[k], polygon[k + 1]];
            if !triangle.contains(&anchor) {
                let sub = [anchor, triangle[0], triangle[1], triangle[2]];
                let o = positions[anchor];
                let det =
                    lu_det(std::array::from_fn(|r| std::array::from_fn(|c| positions[sub[r + 1]][c] - o[c])));
                if det < 0.0 {
                    return Err(err("inconsistent clipped material boundary orientation"));
                }
                if det > 0.0 {
                    subtets.push(sub);
                }
            }
        }
    }
    Ok((subtets, interface))
}

#[must_use]
pub fn subtet_barycentric<S: Scalar>(
    local_values: &[S; 4],
    crossing: &[bool; 6],
    subtets: &[[usize; 4]],
) -> Vec<[[S; 4]; 4]> {
    let identity =
        |i: usize| -> [S; 4] { std::array::from_fn(|k| if k == i { S::one() } else { S::zero() }) };
    let mut table: Vec<[S; 4]> = (0..4).map(identity).collect();
    for (e, &(i, k)) in EDGES.iter().enumerate() {
        let t = if crossing[e] { local_values[i] / (local_values[i] - local_values[k]) } else { S::zero() };
        let (a, b) = (identity(i), identity(k));
        table.push(std::array::from_fn(|c| (S::one() - t) * a[c] + t * b[c]));
    }
    subtets.iter().map(|s| s.map(|v| table[v])).collect()
}

fn det3<S: Scalar>(e: [[S; 3]; 3]) -> S {
    e[0][0] * (e[1][1] * e[2][2] - e[1][2] * e[2][1]) - e[0][1] * (e[1][0] * e[2][2] - e[1][2] * e[2][0])
        + e[0][2] * (e[1][0] * e[2][1] - e[1][1] * e[2][0])
}

fn subtet_fraction<S: Scalar>(b: &[[S; 4]; 4]) -> S {
    det3(std::array::from_fn(|r| std::array::from_fn(|c| b[r + 1][c + 1] - b[0][c + 1])))
}

#[must_use]
pub fn fraction<S: Scalar>(local_values: &[S; 4], crossing: &[bool; 6], subtets: &[[usize; 4]]) -> S {
    subtet_barycentric(local_values, crossing, subtets)
        .iter()
        .fold(S::zero(), |acc, b| acc + subtet_fraction(b))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapeIntegrals<S> {
    pub first_m3: Vec<[S; 4]>,
    pub second_m3: Vec<[[S; 4]; 4]>,
    pub subtet_volume_m3: Vec<Vec<S>>,
}

#[derive(Clone, Debug)]
pub struct ImplicitMaterialSurface {
    geometry: Arc<TetrahedralAleGeometry>,
    reference_levelset: Vec<f64>,
    positive: Vec<bool>,
    edges: Vec<[usize; 2]>,
    subtets: Vec<Vec<[usize; 4]>>,
    triangles: Vec<[usize; 3]>,
    triangle_cells: Vec<usize>,
    crossing: Vec<[bool; 6]>,
    active_cells: Vec<usize>,
    supported_nodes: Vec<usize>,
    unsupported_nodes: Vec<usize>,
}

impl ImplicitMaterialSurface {


    pub fn new(geometry: Arc<TetrahedralAleGeometry>, reference_levelset: &[f64]) -> CaeResult<Self> {
        real_array(reference_levelset, "reference_levelset")?;
        let nn = geometry.node_count();
        if reference_levelset.len() != nn {
            return Err(err(format!(
                "reference_levelset must have shape ({nn},), got ({},)",
                reference_levelset.len()
            )));
        }
        #[allow(clippy::float_cmp)]
        if reference_levelset.contains(&0.0) {
            return Err(err(THROUGH_NODE));
        }
        let positive: Vec<bool> = reference_levelset.iter().map(|v| *v > 0.0).collect();
        let cells = geometry.cells().to_vec();
        let mut crossing_edges = std::collections::BTreeSet::new();
        for c in &cells {
            for &(i, j) in &EDGES {
                if positive[c[i]] != positive[c[j]] {
                    crossing_edges.insert([c[i].min(c[j]), c[i].max(c[j])]);
                }
            }
        }
        let edges: Vec<[usize; 2]> = crossing_edges.into_iter().collect();
        let edge_ids: BTreeMap<[usize; 2], usize> = edges.iter().enumerate().map(|(k, e)| (*e, k)).collect();
        let edge_of = |c: &[usize; 4], (i, j): (usize, usize)| [c[i].min(c[j]), c[i].max(c[j])];
        let (mut subtets, mut triangles, mut owners) = (Vec::new(), Vec::new(), Vec::new());
        for (e, c) in cells.iter().enumerate() {
            let ranks: [usize; 10] = std::array::from_fn(|k| {
                if k < 4 {
                    c[k]
                } else {
                    nn + edge_ids.get(&edge_of(c, EDGES[k - 4])).copied().unwrap_or(edge_ids.len())
                }
            });
            let (sub, interface) = cut_template(c.map(|i| reference_levelset[i]), ranks)?;
            subtets.push(sub);
            for tri in interface {
                let mut ids = [0; 3];
                for (slot, t) in ids.iter_mut().zip(tri) {
                    *slot = edge_ids
                        .get(&edge_of(c, EDGES[t - 4]))
                        .copied()
                        .ok_or_else(|| err("inconsistent clipped material boundary orientation"))?;
                }
                triangles.push(ids);
                owners.push(e);
            }
        }
        let crossing: Vec<[bool; 6]> = cells
            .iter()
            .map(|c| std::array::from_fn(|k| positive[c[EDGES[k].0]] != positive[c[EDGES[k].1]]))
            .collect();
        let active_cells: Vec<usize> =
            (0..cells.len()).filter(|&e| cells[e].iter().any(|&i| positive[i])).collect();
        let supported: std::collections::BTreeSet<usize> =
            active_cells.iter().flat_map(|&e| cells[e]).collect();
        let unsupported_nodes = (0..nn).filter(|i| !supported.contains(i)).collect();
        Ok(Self {
            geometry,
            reference_levelset: reference_levelset.to_vec(),
            positive,
            edges,
            subtets,
            triangles,
            triangle_cells: owners,
            crossing,
            active_cells,
            supported_nodes: supported.into_iter().collect(),
            unsupported_nodes,
        })
    }

    #[must_use]
    pub fn geometry(&self) -> &Arc<TetrahedralAleGeometry> {
        &self.geometry
    }

    #[must_use]
    pub fn reference_levelset(&self) -> &[f64] {
        &self.reference_levelset
    }

    #[must_use]
    pub fn edges(&self) -> &[[usize; 2]] {
        &self.edges
    }

    #[must_use]
    pub fn subtets(&self) -> &[Vec<[usize; 4]>] {
        &self.subtets
    }

    #[must_use]
    pub fn crossing(&self) -> &[[bool; 6]] {
        &self.crossing
    }

    #[must_use]
    pub fn triangles(&self) -> &[[usize; 3]] {
        &self.triangles
    }

    #[must_use]
    pub fn triangle_cells(&self) -> &[usize] {
        &self.triangle_cells
    }

    #[must_use]
    pub fn active_cells(&self) -> &[usize] {
        &self.active_cells
    }

    #[must_use]
    pub fn supported_nodes(&self) -> &[usize] {
        &self.supported_nodes
    }

    #[must_use]
    pub fn unsupported_nodes(&self) -> &[usize] {
        &self.unsupported_nodes
    }



    pub fn validate_levelset(&self, levelset: &[f64]) -> CaeResult<Value> {
        real_array(levelset, "reference_levelset")?;
        let nn = self.geometry.node_count();
        if levelset.len() != nn {
            return Err(err(format!(
                "reference_levelset must have shape ({nn},), got ({},)",
                levelset.len()
            )));
        }
        #[allow(clippy::float_cmp)]
        let changed: Vec<String> = (0..nn)
            .filter(|&i| levelset[i] == 0.0 || (levelset[i] > 0.0) != self.positive[i])
            .map(|i| i.to_string())
            .collect();
        if !changed.is_empty() {
            return Err(err(format!(
                "{SIGN_CHANGED}[{}]; rebuild before solving/linearizing the trial",
                changed.join(", ")
            )));
        }
        Ok(json!({"sign_topology_unchanged": true, "shape_derivatives_valid_within_sign_region": true,
            "material_qualification": false, "engineering_acceptance_performed": false}))
    }



    pub fn interpolation(&self, levelset: &[f64]) -> CaeResult<CsrMatrix> {
        self.validate_levelset(levelset)?;
        let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
        for (k, [a, b]) in self.edges.iter().enumerate() {
            let t = levelset[*a] / (levelset[*a] - levelset[*b]);
            rows.extend([k, k]);
            cols.extend([*a, *b]);
            vals.extend([1.0 - t, t]);
        }
        CsrMatrix::from_triplets(self.edges.len(), self.geometry.node_count(), &rows, &cols, &vals)
            .map_err(|e| err(e.to_string()))
    }



    pub fn trace<S: Scalar>(&self, node_values: &[S], width: usize, levelset: &[S]) -> CaeResult<Vec<S>> {
        let nn = self.geometry.node_count();
        if node_values.len() != nn * width || levelset.len() != nn {
            return Err(err("node trace / reference level-set dimensions do not match"));
        }
        let mut out = Vec::with_capacity(self.edges.len() * width);
        for [a, b] in &self.edges {
            let t = levelset[*a] / (levelset[*a] - levelset[*b]);
            for c in 0..width {
                out.push((S::one() - t) * node_values[a * width + c] + t * node_values[b * width + c]);
            }
        }
        Ok(out)
    }



    pub fn trace_points<S: Scalar>(&self, points: &[Point<S>], levelset: &[S]) -> CaeResult<Vec<Point<S>>> {
        let flat: Vec<S> = points.iter().flatten().copied().collect();
        Ok(self.trace(&flat, 3, levelset)?.chunks(3).map(|c| [c[0], c[1], c[2]]).collect())
    }



    pub fn transpose<S: Scalar>(
        &self,
        surface_values: &[S],
        width: usize,
        levelset: &[S],
    ) -> CaeResult<Vec<S>> {
        if surface_values.len() != self.edges.len() * width {
            return Err(err("one value per material cut vertex is required"));
        }
        let mut out = vec![S::zero(); self.geometry.node_count() * width];
        let ts: Vec<S> =
            self.edges.iter().map(|[a, b]| levelset[*a] / (levelset[*a] - levelset[*b])).collect();
        for (k, [a, _]) in self.edges.iter().enumerate() {
            for c in 0..width {
                out[a * width + c] += (S::one() - ts[k]) * surface_values[k * width + c];
            }
        }
        for (k, [_, b]) in self.edges.iter().enumerate() {
            for c in 0..width {
                out[b * width + c] += ts[k] * surface_values[k * width + c];
            }
        }
        Ok(out)
    }

    fn local<S: Scalar>(&self, levelset: &[S], e: usize) -> [S; 4] {
        self.geometry.cells()[e].map(|i| levelset[i])
    }

    #[must_use]
    pub fn volume_fractions<S: Scalar>(&self, levelset: &[S]) -> Vec<S> {
        (0..self.geometry.cell_count())
            .map(|e| fraction(&self.local(levelset, e), &self.crossing[e], &self.subtets[e]))
            .collect()
    }

    #[must_use]
    pub fn material_shape_integrals<S: Scalar>(&self, levelset: &[S]) -> ShapeIntegrals<S> {
        let parent = self.geometry.volumes(self.geometry.reference_points_m());
        let mut out =
            ShapeIntegrals { first_m3: Vec::new(), second_m3: Vec::new(), subtet_volume_m3: Vec::new() };
        for (e, parent_volume) in parent.iter().enumerate() {
            let bary = subtet_barycentric(&self.local(levelset, e), &self.crossing[e], &self.subtets[e]);
            let mut first = [S::zero(); 4];
            let mut second = [[S::zero(); 4]; 4];
            let mut volumes = Vec::with_capacity(bary.len());
            for b in &bary {
                let volume = subtet_fraction(b) * *parent_volume;
                let sums: [S; 4] = std::array::from_fn(|i| b[0][i] + b[1][i] + b[2][i] + b[3][i]);
                for i in 0..4 {
                    first[i] += volume * sums[i] / 4.0;
                    for j in 0..4 {
                        let outer = (0..4).fold(S::zero(), |acc, a| acc + b[a][i] * b[a][j]);
                        second[i][j] += volume * (sums[i] * sums[j] + outer) / 20.0;
                    }
                }
                volumes.push(volume);
            }
            out.first_m3.push(first);
            out.second_m3.push(second);
            out.subtet_volume_m3.push(volumes);
        }
        out
    }

    #[must_use]
    pub fn material_volumes_m3<S: Scalar>(&self, current_points: &[Point<S>], levelset: &[S]) -> Vec<S> {
        self.volume_fractions(levelset)
            .iter()
            .zip(self.geometry.volumes(current_points))
            .map(|(f, v)| *f * v)
            .collect()
    }

    #[must_use]
    pub fn material_mass_kg<S: Scalar>(&self, reference_density: &[S], levelset: &[S]) -> Vec<S> {
        let reference: Vec<Point<S>> =
            self.geometry.reference_points_m().iter().map(|p| p.map(S::from_f64)).collect();
        self.material_volumes_m3(&reference, levelset)
            .iter()
            .enumerate()
            .map(|(e, v)| reference_density[if reference_density.len() == 1 { 0 } else { e }] * *v)
            .collect()
    }



    pub fn trace_partials(
        &self,
        node_values: &[f64],
        width: usize,
        levelset: &[f64],
    ) -> CaeResult<(CsrMatrix, CsrMatrix)> {
        real_array(node_values, "node_values")?;
        let nn = self.geometry.node_count();
        if width == 0 || node_values.len() != nn * width {
            return Err(err("trace partials require [node,component]"));
        }
        let h = self.interpolation(levelset)?;
        let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..h.nrows() {
            let (idx, data) = h.row(i);
            for c in 0..width {
                for (j, v) in idx.iter().zip(data) {
                    rows.push(i * width + c);
                    cols.push(j * width + c);
                    vals.push(*v);
                }
            }
        }
        let kron = CsrMatrix::from_triplets(h.nrows() * width, nn * width, &rows, &cols, &vals)
            .map_err(|e| err(e.to_string()))?;
        let mut local = Vec::with_capacity(self.edges.len() * width * 2);
        let mut columns = Vec::with_capacity(self.edges.len() * 2);
        for [a, b] in &self.edges {
            let den = (levelset[*a] - levelset[*b]).powi(2);
            let dt = [-levelset[*b] / den, levelset[*a] / den];
            for c in 0..width {
                let delta = node_values[b * width + c] - node_values[a * width + c];
                local.extend([delta * dt[0], delta * dt[1]]);
            }
            columns.extend([*a, *b]);
        }
        let dpsi = assemble_entity_blocks(&local, &columns, 2, width, nn)?;
        Ok((kron, dpsi))
    }



    pub fn volume_fraction_partials(&self, levelset: &[f64]) -> CaeResult<CsrMatrix> {
        self.validate_levelset(levelset)?;
        let mut values = Vec::with_capacity(4 * self.geometry.cell_count());
        let mut columns = Vec::with_capacity(4 * self.geometry.cell_count());
        for (e, c) in self.geometry.cells().iter().enumerate() {
            let local: [Dual<4>; 4] = std::array::from_fn(|k| Dual::variable(levelset[c[k]], k));
            values.extend(fraction(&local, &self.crossing[e], &self.subtets[e]).eps);
            columns.extend(c);
        }
        assemble_entity_blocks(&values, &columns, 4, 1, self.geometry.node_count())
    }

    #[must_use]
    pub fn metadata(&self) -> Value {
        json!({"schema": "implexity-material-levelset-trace/1", "sign_convention": "positive_solid",
            "levelset_frame": "material_reference", "moving_surface": true, "finite_motion": true,
            "interpolation_and_volume_shape_partials": true, "frozen_sign_topology": true,
            "topology_change_policy": "recoverable_rebuild_and_relinearization",
            "interface_triangles": self.triangles.len(), "active_tetrahedra": self.active_cells.len(),
            "unsupported_nodes": self.unsupported_nodes, "numerical_void_material_used": false,
            "contact_checked": false, "conditioning_or_resolution_qualified": false, "physical_qualification": false})
    }
}

