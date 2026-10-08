// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use implexity_ad::{Dual, Scalar};
use implexity_core::error::{CaeError, CaeResult};
use implexity_core::py_repr::repr_float;
use implexity_linalg::sparse::CsrMatrix;
use serde_json::{Value, json};

use crate::matrix::eliminate_zeros;

pub(crate) fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

pub type Point<S> = [S; 3];



pub fn real_array(values: &[f64], name: &str) -> CaeResult<()> {
    if values.iter().all(Scalar::is_finite) {
        Ok(())
    } else {
        Err(err(format!("{name} must contain finite real values, not booleans")))
    }
}



pub fn positive_step(step: f64) -> CaeResult<f64> {
    real_array(&[step], "step_s")?;
    if step <= 0.0 {
        return Err(err("step_s must be positive"));
    }
    Ok(step)
}

fn sub<S: Scalar>(a: Point<S>, b: Point<S>) -> Point<S> {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross<S: Scalar>(a: Point<S>, b: Point<S>) -> Point<S> {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[must_use]
pub fn triangle_area_vector<S: Scalar>(x: &[Point<S>; 3]) -> Point<S> {
    let c = cross(sub(x[1], x[0]), sub(x[2], x[0]));
    [c[0] * 0.5, c[1] * 0.5, c[2] * 0.5]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriangleMetrics<S> {
    pub area_start_m2: Point<S>,
    pub area_mid_m2: Point<S>,
    pub area_end_m2: Point<S>,
    pub area_average_m2: Point<S>,
    pub swept_volume_m3: S,
}

#[must_use]
pub fn triangle_metrics<S: Scalar>(x0: &[Point<S>; 3], x1: &[Point<S>; 3]) -> TriangleMetrics<S> {
    let a0 = triangle_area_vector(x0);
    let mid: [Point<S>; 3] = std::array::from_fn(|v| std::array::from_fn(|k| (x0[v][k] + x1[v][k]) * 0.5));
    let am = triangle_area_vector(&mid);
    let a1 = triangle_area_vector(x1);
    let average: Point<S> = std::array::from_fn(|k| (a0[k] + am[k] * 4.0 + a1[k]) / 6.0);
    let delta: Point<S> = std::array::from_fn(|k| {
        ((x1[0][k] - x0[0][k]) + (x1[1][k] - x0[1][k]) + (x1[2][k] - x0[2][k])) / 3.0
    });
    let swept = average[0] * delta[0] + average[1] * delta[1] + average[2] * delta[2];
    TriangleMetrics {
        area_start_m2: a0,
        area_mid_m2: am,
        area_end_m2: a1,
        area_average_m2: average,
        swept_volume_m3: swept,
    }
}

fn det3<S: Scalar>(e: [Point<S>; 3]) -> S {
    e[0][0] * (e[1][1] * e[2][2] - e[1][2] * e[2][1]) - e[0][1] * (e[1][0] * e[2][2] - e[1][2] * e[2][0])
        + e[0][2] * (e[1][0] * e[2][1] - e[1][1] * e[2][0])
}

#[must_use]
pub fn tetra_volume<S: Scalar>(x: &[Point<S>; 4]) -> S {
    det3([sub(x[1], x[0]), sub(x[2], x[0]), sub(x[3], x[0])]) / 6.0
}

#[derive(Clone, Debug, PartialEq)]
pub struct GeometryMetrics<S> {
    pub faces: Vec<TriangleMetrics<S>>,
    pub volume_start_m3: Vec<S>,
    pub volume_end_m3: Vec<S>,
    pub gcl_error_m3: Vec<S>,
    pub surface_closure_m2: Vec<Point<S>>,
}

impl<S: Scalar> GeometryMetrics<S> {
    #[must_use]
    pub fn swept_volume_m3(&self) -> Vec<S> {
        self.faces.iter().map(|f| f.swept_volume_m3).collect()
    }

    #[must_use]
    pub fn area_average_m2(&self) -> Vec<Point<S>> {
        self.faces.iter().map(|f| f.area_average_m2).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EndpointPartials {
    pub area_average_m2: CsrMatrix,
    pub swept_volume_m3: CsrMatrix,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GeometryPartials {
    pub previous_points_m: EndpointPartials,
    pub current_points_m: EndpointPartials,
    pub volume_start_m3: CsrMatrix,
    pub volume_end_m3: CsrMatrix,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MotionReport {
    pub minimum_path_volume_m3: f64,
    pub minimum_location: Option<(usize, f64)>,
    pub maximum_abs_gcl_error_m3: f64,
    pub maximum_abs_surface_closure_m2: f64,
}

impl MotionReport {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "minimum_path_volume_m3": self.minimum_path_volume_m3,
            "minimum_location": self.minimum_location.map(|(c, t)| json!([c, t])),
            "maximum_abs_gcl_error_m3": self.maximum_abs_gcl_error_m3,
            "maximum_abs_surface_closure_m2": self.maximum_abs_surface_closure_m2,
            "connectivity_unchanged": true, "nonlocal_contact_checked": false,
            "motion_path": "straight_vertex_segments", "physical_qualification": false,
        })
    }
}



pub fn assemble_entity_blocks(
    values: &[f64],
    columns: &[usize],
    width: usize,
    output_size: usize,
    input_size: usize,
) -> CaeResult<CsrMatrix> {
    let count = columns.len() / width.max(1);
    let (mut rows, mut cols) = (Vec::with_capacity(values.len()), Vec::with_capacity(values.len()));
    for e in 0..count {
        for o in 0..output_size {
            for w in 0..width {
                rows.push(output_size * e + o);
                cols.push(columns[e * width + w]);
            }
        }
    }
    let m = CsrMatrix::from_triplets(count * output_size, input_size, &rows, &cols, values)
        .map_err(|e| err(e.to_string()))?;
    Ok(eliminate_zeros(&m))
}

pub(crate) fn select_rows(m: &CsrMatrix, select: &[usize]) -> CaeResult<CsrMatrix> {
    let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
    for (r, &i) in select.iter().enumerate() {
        let (idx, data) = m.row(i);
        for (c, v) in idx.iter().zip(data) {
            rows.push(r);
            cols.push(*c);
            vals.push(*v);
        }
    }
    CsrMatrix::from_triplets(select.len(), m.ncols(), &rows, &cols, &vals).map_err(|e| err(e.to_string()))
}

#[derive(Clone, Debug)]
pub struct TetrahedralAleGeometry {
    reference_points_m: Vec<Point<f64>>,
    cells: Vec<[usize; 4]>,
    faces: Vec<[usize; 3]>,
    left: Vec<usize>,
    right: Vec<Option<usize>>,
    boundary_faces: Vec<usize>,
    interior: Vec<usize>,
    incidence: CsrMatrix,
}

const OUTWARD: [[usize; 3]; 4] = [[1, 2, 3], [0, 3, 2], [0, 1, 3], [0, 2, 1]];

impl TetrahedralAleGeometry {


    pub fn new(reference_points_m: &[Point<f64>], tetrahedra: &[[i64; 4]]) -> CaeResult<Self> {
        real_array(&reference_points_m.concat(), "reference_points_m")?;
        let n = reference_points_m.len();
        if n < 4 {
            return Err(err("at least four XYZ reference nodes required"));
        }
        if tetrahedra.is_empty() {
            return Err(err("nonempty integer [cell,4] tetrahedra required"));
        }
        if tetrahedra.iter().flatten().any(|&i| i < 0 || usize::try_from(i).map_or(true, |i| i >= n)) {
            return Err(err("tetrahedron index outside the reference nodes"));
        }
        let cells: Vec<[usize; 4]> =
            tetrahedra.iter().map(|c| c.map(|i| usize::try_from(i).unwrap_or(usize::MAX))).collect();
        if cells.iter().any(|c| {
            let mut s = *c;
            s.sort_unstable();
            s.windows(2).any(|w| w[0] == w[1])
        }) {
            return Err(err("a tetrahedron contains repeated vertices"));
        }
        let mut keys: Vec<[usize; 4]> = cells
            .iter()
            .map(|c| {
                let mut s = *c;
                s.sort_unstable();
                s
            })
            .collect();
        keys.sort_unstable();
        keys.dedup();
        if keys.len() != cells.len() {
            return Err(err("duplicate tetrahedra are not independent volumes"));
        }
        for c in &cells {
            let v = tetra_volume(&c.map(|i| reference_points_m[i]));
            if !v.is_finite() || v <= 0.0 {
                return Err(err("positive oriented reference tetrahedra required"));
            }
        }
        let mut faces: Vec<[usize; 3]> = Vec::new();
        let (mut left, mut right): (Vec<usize>, Vec<Option<usize>>) = (Vec::new(), Vec::new());
        let mut lookup: std::collections::HashMap<[usize; 3], usize> = std::collections::HashMap::new();
        for (cell_id, cell) in cells.iter().enumerate() {
            for local in OUTWARD {
                let face = local.map(|k| cell[k]);
                let mut key = face;
                key.sort_unstable();
                if let Some(&index) = lookup.get(&key) {
                    if right[index].is_some() {
                        return Err(err("nonmanifold face has more than two cells"));
                    }
                    let previous: [usize; 3] = faces[index];
                    if (0..3).any(|k| face == [previous[k], previous[(k + 1) % 3], previous[(k + 2) % 3]]) {
                        return Err(err("shared face orientations must oppose"));
                    }
                    right[index] = Some(cell_id);
                } else {
                    lookup.insert(key, faces.len());
                    faces.push(face);
                    left.push(cell_id);
                    right.push(None);
                }
            }
        }
        let boundary_faces: Vec<usize> = (0..faces.len()).filter(|&f| right[f].is_none()).collect();
        let interior: Vec<usize> = (0..faces.len()).filter(|&f| right[f].is_some()).collect();
        let (mut rows, mut cols, mut vals) =
            (left.clone(), (0..faces.len()).collect::<Vec<_>>(), vec![1.0; faces.len()]);
        for &f in &interior {
            rows.push(right[f].unwrap_or(0));
            cols.push(f);
            vals.push(-1.0);
        }
        let incidence = CsrMatrix::from_triplets(cells.len(), faces.len(), &rows, &cols, &vals)
            .map_err(|e| err(e.to_string()))?;
        Ok(Self {
            reference_points_m: reference_points_m.to_vec(),
            cells,
            faces,
            left,
            right,
            boundary_faces,
            interior,
            incidence,
        })
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.reference_points_m.len()
    }

    #[must_use]
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    #[must_use]
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    #[must_use]
    pub fn reference_points_m(&self) -> &[Point<f64>] {
        &self.reference_points_m
    }

    #[must_use]
    pub fn cells(&self) -> &[[usize; 4]] {
        &self.cells
    }

    #[must_use]
    pub fn faces(&self) -> &[[usize; 3]] {
        &self.faces
    }

    #[must_use]
    pub fn left(&self) -> &[usize] {
        &self.left
    }

    #[must_use]
    pub fn right(&self) -> &[Option<usize>] {
        &self.right
    }

    #[must_use]
    pub fn boundary_faces(&self) -> &[usize] {
        &self.boundary_faces
    }

    #[must_use]
    pub fn interior_faces(&self) -> &[usize] {
        &self.interior
    }

    #[must_use]
    pub fn incidence(&self) -> &CsrMatrix {
        &self.incidence
    }



    pub fn divergence<S: Scalar>(&self, face_values: &[S], width: usize) -> CaeResult<Vec<S>> {
        if face_values.len() != self.faces.len() * width {
            return Err(err("face values must match the geometry topology"));
        }
        let mut out = vec![S::zero(); self.cells.len() * width];
        for (f, &l) in self.left.iter().enumerate() {
            for c in 0..width {
                out[l * width + c] += face_values[f * width + c];
            }
        }
        for &f in &self.interior {
            if let Some(r) = self.right[f] {
                for c in 0..width {
                    out[r * width + c] -= face_values[f * width + c];
                }
            }
        }
        Ok(out)
    }

    #[must_use]
    pub fn volumes<S: Scalar>(&self, points: &[Point<S>]) -> Vec<S> {
        self.cells.iter().map(|c| tetra_volume(&c.map(|i| points[i]))).collect()
    }

    fn check_points<S: Scalar>(&self, x0: &[Point<S>], x1: &[Point<S>]) -> CaeResult<()> {
        if x0.len() != self.node_count() || x1.len() != x0.len() {
            return Err(err("matching node-by-XYZ positions required"));
        }
        Ok(())
    }



    pub fn metrics<S: Scalar>(&self, x0: &[Point<S>], x1: &[Point<S>]) -> CaeResult<GeometryMetrics<S>> {
        self.check_points(x0, x1)?;
        let faces: Vec<TriangleMetrics<S>> =
            self.faces.iter().map(|f| triangle_metrics(&f.map(|i| x0[i]), &f.map(|i| x1[i]))).collect();
        let v0 = self.volumes(x0);
        let v1 = self.volumes(x1);
        let swept: Vec<S> = faces.iter().map(|m| m.swept_volume_m3).collect();
        let div = self.divergence(&swept, 1)?;
        let gcl = v0.iter().zip(&v1).zip(&div).map(|((a, b), d)| *b - *a - *d).collect();
        let areas: Vec<S> = faces.iter().flat_map(|m| m.area_average_m2).collect();
        let closure = self.divergence(&areas, 3)?.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
        Ok(GeometryMetrics {
            faces,
            volume_start_m3: v0,
            volume_end_m3: v1,
            gcl_error_m3: gcl,
            surface_closure_m2: closure,
        })
    }



    pub fn validate_motion(&self, x0: &[Point<f64>], x1: &[Point<f64>]) -> CaeResult<MotionReport> {
        real_array(&x0.concat(), "previous_points_m")?;
        real_array(&x1.concat(), "current_points_m")?;
        if x0.len() != self.node_count() {
            return Err(err(format!(
                "previous_points_m must have shape ({}, 3), got ({}, 3)",
                self.node_count(),
                x0.len()
            )));
        }
        if x1.len() != self.node_count() {
            return Err(err(format!(
                "current_points_m must have shape ({}, 3), got ({}, 3)",
                self.node_count(),
                x1.len()
            )));
        }
        let mut minimum = f64::INFINITY;
        let mut when = None;
        for (cell, c) in self.cells.iter().enumerate() {
            let e0: [Point<f64>; 3] = std::array::from_fn(|k| sub(x0[c[k + 1]], x0[c[0]]));
            let de: [Point<f64>; 3] = std::array::from_fn(|k| sub(sub(x1[c[k + 1]], x1[c[0]]), e0[k]));
            let mut coefficients = [0.0; 4];
            for bits in 0..8usize {
                let pick: [bool; 3] = [bits & 4 != 0, bits & 2 != 0, bits & 1 != 0];
                let e: [Point<f64>; 3] = std::array::from_fn(|k| if pick[k] { de[k] } else { e0[k] });
                coefficients[pick.iter().filter(|b| **b).count()] += det3(e) / 6.0;
            }
            let mut times = vec![0.0, 1.0];
            times.extend(
                real_roots([coefficients[1], 2.0 * coefficients[2], 3.0 * coefficients[3]])
                    .into_iter()
                    .filter(|t| *t > 0.0 && *t < 1.0),
            );
            for t in times {
                let e: [Point<f64>; 3] =
                    std::array::from_fn(|k| std::array::from_fn(|j| e0[k][j] + t * de[k][j]));
                let volume = det3(e) / 6.0;
                if volume < minimum {
                    minimum = volume;
                    when = Some((cell, t));
                }
            }
        }
        if !minimum.is_finite() || minimum <= 0.0 {
            let location =
                when.map_or_else(|| "None".to_string(), |(c, t)| format!("({c}, {})", repr_float(t)));
            return Err(err(format!(
                "nonpositive cell volume along motion path at {location}: {}",
                repr_float(minimum)
            )));
        }
        let m = self.metrics(x0, x1)?;
        let max_abs = |v: &mut dyn Iterator<Item = f64>| v.map(f64::abs).fold(f64::NEG_INFINITY, f64::max);
        Ok(MotionReport {
            minimum_path_volume_m3: minimum,
            minimum_location: when,
            maximum_abs_gcl_error_m3: max_abs(&mut m.gcl_error_m3.iter().copied()),
            maximum_abs_surface_closure_m2: max_abs(&mut m.surface_closure_m2.iter().flatten().copied()),
        })
    }



    pub fn sparse_partials(&self, x0: &[Point<f64>], x1: &[Point<f64>]) -> CaeResult<GeometryPartials> {
        real_array(&x0.concat(), "previous_points_m")?;
        real_array(&x1.concat(), "current_points_m")?;
        self.check_points(x0, x1)?;
        let nf = self.faces.len();
        let nn3 = 3 * self.node_count();
        let columns: Vec<usize> =
            self.faces.iter().flat_map(|f| f.iter().flat_map(|&i| [3 * i, 3 * i + 1, 3 * i + 2])).collect();
        let mut blocks = [Vec::with_capacity(nf * 36), Vec::with_capacity(nf * 36)];
        for f in &self.faces {
            for (arg, block) in blocks.iter_mut().enumerate() {
                let seed = |x: &[Point<f64>], active: bool| -> [Point<Dual<9>>; 3] {
                    std::array::from_fn(|v| {
                        std::array::from_fn(|k| {
                            let value = x[f[v]][k];
                            if active { Dual::variable(value, 3 * v + k) } else { Dual::constant(value) }
                        })
                    })
                };
                let m = triangle_metrics(&seed(x0, arg == 0), &seed(x1, arg == 1));
                for out in
                    [m.area_average_m2[0], m.area_average_m2[1], m.area_average_m2[2], m.swept_volume_m3]
                {
                    block.extend_from_slice(&out.eps);
                }
            }
        }
        let area_rows: Vec<usize> = (0..nf).flat_map(|f| [4 * f, 4 * f + 1, 4 * f + 2]).collect();
        let swept_rows: Vec<usize> = (0..nf).map(|f| 4 * f + 3).collect();
        let endpoint = |block: &[f64]| -> CaeResult<EndpointPartials> {
            let matrix = assemble_entity_blocks(block, &columns, 9, 4, nn3)?;
            Ok(EndpointPartials {
                area_average_m2: select_rows(&matrix, &area_rows)?,
                swept_volume_m3: select_rows(&matrix, &swept_rows)?,
            })
        };
        let vcols: Vec<usize> =
            self.cells.iter().flat_map(|c| c.iter().flat_map(|&i| [3 * i, 3 * i + 1, 3 * i + 2])).collect();
        let volume_block = |x: &[Point<f64>]| -> Vec<f64> {
            self.cells
                .iter()
                .flat_map(|c| {
                    let p: [Point<Dual<12>>; 4] = std::array::from_fn(|v| {
                        std::array::from_fn(|k| Dual::variable(x[c[v]][k], 3 * v + k))
                    });
                    tetra_volume(&p).eps
                })
                .collect()
        };
        Ok(GeometryPartials {
            previous_points_m: endpoint(&blocks[0])?,
            current_points_m: endpoint(&blocks[1])?,
            volume_start_m3: assemble_entity_blocks(&volume_block(x0), &vcols, 12, 1, nn3)?,
            volume_end_m3: assemble_entity_blocks(&volume_block(x1), &vcols, 12, 1, nn3)?,
        })
    }

    #[must_use]
    pub fn metadata(&self) -> Value {
        json!({"schema": "implexity-ale-geometry/1", "geometry_moves": true,
            "topology": "conforming_tetrahedra", "fixed_connectivity": true,
            "face_metric": "exact_Simpson_for_straight_vertex_paths",
            "source_owned_sparse_geometry_partials": true,
            "nonlocal_contact_checked": false, "fluid_solver_installed_by_this_operator": false,
            "physical_qualification": false})
    }
}

fn real_roots(c: [f64; 3]) -> Vec<f64> {
    let [c0, c1, c2] = c;
    #[allow(clippy::float_cmp)]
    if c2 == 0.0 {
        #[allow(clippy::float_cmp)]
        if c1 == 0.0 {
            return Vec::new();
        }
        return vec![-c0 / c1];
    }
    let tol = |re: f64| 64.0 * f64::EPSILON * re.abs().max(1.0);
    let disc = c1 * c1 - 4.0 * c2 * c0;
    if disc >= 0.0 {
        let q = -0.5 * (c1 + c1.signum() * disc.sqrt());
        #[allow(clippy::float_cmp)]
        if q == 0.0 {
            return vec![0.0, 0.0];
        }
        vec![q / c2, c0 / q]
    } else {
        let re = -c1 / (2.0 * c2);
        let im = (-disc).sqrt() / (2.0 * c2.abs());
        if im <= tol(re) { vec![re, re] } else { Vec::new() }
    }
}

