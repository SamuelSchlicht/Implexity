// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::ale_geometry::{Point, TetrahedralAleGeometry, positive_step, real_array};
use implexity_solve::implicit_material_surface::ImplicitMaterialSurface;
use implexity_solve::material_cut_ale::{MaterialCutAle, Phase};

use crate::implicit_hyperelastic_support::ImplicitHyperelasticSupport;

pub const COMPONENT_KIND: &str = "moving_material_geometry_and_work";
pub const IMPLEMENTATION: &str =
    "implexity.physics_library.moving_material_components.MovingMaterialOperators";
pub const LIMITATIONS: [&str; 4] = [
    "Finite-motion geometry and material-cut transport, not an autonomous FSI field solver.",
    "Host must solve consistent fluid and finite-strain solid fields and chain constitutive derivatives.",
    "Fixed sign topology per linearization; conservative topology-change history remap is not installed.",
    "No contact, geometric-resolution, constitutive-calibration or Windows qualification is implied.",
];

#[must_use]
pub fn runtime_support() -> Map<String, Value> {
    crate::util::obj(json!({"status": "field_component", "history": false, "limitations": LIMITATIONS}))
}

#[must_use]
pub fn exchange_metadata() -> Value {
    json!({"schema": "implexity-moving-material-exchange/1", "finite_motion": true,
        "normal": "out_of_fluid_into_solid", "reference_domain_mass_compensation": false,
        "pressure": "physical_internal_absolute_not_pressure_drop", "work": "reciprocal_total_energy_source_not_heat",
        "exterior_load_requires_own_authored_surface": true, "geometry_shape_partials": true,
        "stress_quadrature": "Simpson", "state_and_constitutive_chains_required_from_owner": true,
        "no_slip_or_contact_law_installed": false, "complete_fsi_provider": false, "physical_qualification": false})
}

pub fn triangle_area_vector<S: Scalar>(p: &[[S; 3]; 3]) -> [S; 3] {
    let a: [S; 3] = std::array::from_fn(|i| p[1][i] - p[0][i]);
    let b: [S; 3] = std::array::from_fn(|i| p[2][i] - p[0][i]);
    [(a[1] * b[2] - a[2] * b[1]) * 0.5, (a[2] * b[0] - a[0] * b[2]) * 0.5, (a[0] * b[1] - a[1] * b[0]) * 0.5]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TriangleExchange<S> {
    pub internal_force: [S; 3],
    pub fluid_source: [S; 5],
    pub internal_work: S,
    pub velocity: [S; 3],
    pub pressure_force: [S; 3],
    pub viscous_force: [S; 3],
    pub swept_volume: S,
}


pub fn triangle_exchange<S: Scalar>(
    previous: &[[S; 3]; 3],
    current: &[[S; 3]; 3],
    pressure: &[S; 3],
    viscous: &[[[S; 3]; 3]; 3],
    step: S,
) -> TriangleExchange<S> {
    let mid: [[S; 3]; 3] =
        std::array::from_fn(|v| std::array::from_fn(|i| (previous[v][i] + current[v][i]) * 0.5));
    let areas = [triangle_area_vector(previous), triangle_area_vector(&mid), triangle_area_vector(current)];
    let weights = [1.0 / 6.0, 4.0 / 6.0, 1.0 / 6.0];
    let mut pressure_force = [S::zero(); 3];
    let mut viscous_force = [S::zero(); 3];
    let mut weighted_area = [S::zero(); 3];
    for t in 0..3 {
        for i in 0..3 {
            pressure_force[i] += pressure[t] * weights[t] * areas[t][i];
            let mut traction = S::zero();
            for j in 0..3 {
                traction += viscous[t][i][j] * areas[t][j];
            }
            viscous_force[i] -= traction * weights[t];
            weighted_area[i] += areas[t][i] * weights[t];
        }
    }
    let internal: [S; 3] = std::array::from_fn(|i| pressure_force[i] + viscous_force[i]);
    let delta: [S; 3] = std::array::from_fn(|i| {
        let mut acc = S::zero();
        for v in 0..3 {
            acc += current[v][i] - previous[v][i];
        }
        acc / 3.0
    });
    let velocity: [S; 3] = std::array::from_fn(|i| delta[i] / step);
    let dot = |a: &[S; 3], b: &[S; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let work = dot(&internal, &delta);
    TriangleExchange {
        internal_force: internal,
        fluid_source: [S::zero(), -internal[0], -internal[1], -internal[2], -dot(&internal, &velocity)],
        internal_work: work,
        velocity,
        pressure_force,
        viscous_force,
        swept_volume: dot(&weighted_area, &delta),
    }
}

fn geometry_error(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

pub(crate) fn sparse_sum(
    nrows: usize,
    ncols: usize,
    triplets: impl IntoIterator<Item = (usize, usize, f64)>,
) -> Result<CsrMatrix, CaeError> {
    let mut map: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for (r, c, v) in triplets {
        *map.entry((r, c)).or_insert(0.0) += v;
    }
    let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
    for ((r, c), v) in map {
        if v != 0.0 {
            rows.push(r);
            cols.push(c);
            vals.push(v);
        }
    }
    CsrMatrix::from_triplets(nrows, ncols, &rows, &cols, &vals).map_err(|e| geometry_error(e.to_string()))
}

#[derive(Debug, Clone)]
pub struct InterfaceInterval<S> {
    pub solid_internal_nodal_force: Vec<[S; 3]>,
    pub fluid_conserved_source_rate: Vec<[S; 5]>,
    pub internal_work: Vec<S>,
    pub reciprocal_force_error: [S; 3],
    pub reciprocal_power_error: S,
}

#[derive(Debug, Clone)]
pub struct ExchangePartials {
    pub previous_points: CsrMatrix,
    pub current_points: CsrMatrix,
    pub levelset: CsrMatrix,
    pub pressure: CsrMatrix,
    pub viscous_stress: CsrMatrix,
    pub step: CsrMatrix,
}


#[derive(Debug, Clone)]
pub struct MaterialInterfaceExchange {
    surface: ImplicitMaterialSurface,
    face_count: usize,
    cells: Vec<[usize; 4]>,
    edge_local: Vec<[[usize; 2]; 3]>,
}

fn exchange_local<S: Scalar>(edges: &[[usize; 2]; 3], x: &[S]) -> [S; 18] {
    let point = |base: usize, a: usize| -> [S; 3] { std::array::from_fn(|i| x[base + 3 * a + i]) };
    let psi = &x[24..28];
    let mut h = [[S::zero(); 4]; 3];
    for (k, e) in edges.iter().enumerate() {
        let (a, b) = (e[0], e[1]);
        let t = psi[a] / (psi[a] - psi[b]);
        h[k][a] = -t + 1.0;
        h[k][b] = t;
    }
    let interp = |base: usize| -> [[S; 3]; 3] {
        std::array::from_fn(|k| {
            let mut out = [S::zero(); 3];
            for (a, w) in h[k].iter().enumerate() {
                let p = point(base, a);
                for i in 0..3 {
                    out[i] += *w * p[i];
                }
            }
            out
        })
    };
    let pressure = [x[28], x[29], x[30]];
    let viscous: [[[S; 3]; 3]; 3] =
        std::array::from_fn(|t| std::array::from_fn(|i| std::array::from_fn(|j| x[31 + 9 * t + 3 * i + j])));
    let q = triangle_exchange(&interp(0), &interp(12), &pressure, &viscous, x[58]);
    let mut out = [S::zero(); 18];
    for a in 0..4 {
        for i in 0..3 {
            let mut acc = S::zero();
            for row in &h {
                acc += row[a] * (q.internal_force[i] / 3.0);
            }
            out[3 * a + i] = acc;
        }
    }
    out[12..17].copy_from_slice(&q.fluid_source);
    out[17] = q.internal_work;
    out
}

impl MaterialInterfaceExchange {

    pub fn new(surface: ImplicitMaterialSurface) -> Result<Self, CaeError> {
        let nf = surface.triangles().len();
        if nf < 1 {
            return Err(geometry_error("an actual material interface is required"));
        }
        let geometry = Arc::clone(surface.geometry());
        let cells: Vec<[usize; 4]> = surface.triangle_cells().iter().map(|c| geometry.cells()[*c]).collect();
        let edge_local = cells
            .iter()
            .zip(surface.triangles())
            .map(|(cell, triangle)| {
                triangle.map(|t| surface.edges()[t].map(|v| cell.iter().position(|n| *n == v).unwrap_or(0)))
            })
            .collect();
        Ok(Self { surface, face_count: nf, cells, edge_local })
    }

    #[must_use]
    pub fn face_count(&self) -> usize {
        self.face_count
    }

    #[must_use]
    pub fn surface(&self) -> &ImplicitMaterialSurface {
        &self.surface
    }

    fn local_inputs<S: Scalar>(
        &self,
        f: usize,
        previous: &[Point<S>],
        current: &[Point<S>],
        levelset: &[S],
        pressure: &[[S; 3]],
        viscous: &[[[[S; 3]; 3]; 3]],
        step: S,
    ) -> Vec<S> {
        let cell = &self.cells[f];
        let mut x = Vec::with_capacity(59);
        x.extend(cell.iter().flat_map(|n| previous[*n]));
        x.extend(cell.iter().flat_map(|n| current[*n]));
        x.extend(cell.iter().map(|n| levelset[*n]));
        x.extend(pressure[f]);
        x.extend(viscous[f].iter().flatten().flatten().copied());
        x.push(step);
        x
    }

    pub fn interval<S: Scalar>(
        &self,
        previous: &[Point<S>],
        current: &[Point<S>],
        levelset: &[S],
        pressure: &[[S; 3]],
        viscous: &[[[[S; 3]; 3]; 3]],
        step: S,
    ) -> InterfaceInterval<S> {
        let nn = self.surface.geometry().node_count();
        let mut internal = vec![[S::zero(); 3]; nn];
        let mut sources = Vec::with_capacity(self.face_count);
        let mut work = Vec::with_capacity(self.face_count);
        for f in 0..self.face_count {
            let x = self.local_inputs(f, previous, current, levelset, pressure, viscous, step);
            let row = exchange_local(&self.edge_local[f], &x);
            for (a, node) in self.cells[f].iter().enumerate() {
                for i in 0..3 {
                    internal[*node][i] += row[3 * a + i];
                }
            }
            sources.push([row[12], row[13], row[14], row[15], row[16]]);
            work.push(row[17]);
        }
        let mut force_error = [S::zero(); 3];
        let mut power_error = S::zero();
        for (node, f) in internal.iter().enumerate() {
            for i in 0..3 {
                force_error[i] += f[i];
                power_error += f[i] * ((current[node][i] - previous[node][i]) / step);
            }
        }
        for s in &sources {
            for i in 0..3 {
                force_error[i] += s[1 + i];
            }
            power_error += s[4];
        }
        InterfaceInterval {
            solid_internal_nodal_force: internal,
            fluid_conserved_source_rate: sources,
            internal_work: work,
            reciprocal_force_error: force_error,
            reciprocal_power_error: power_error,
        }
    }


    pub fn validate(
        &self,
        previous: &[Point<f64>],
        current: &[Point<f64>],
        levelset: &[f64],
        pressure: &[[f64; 3]],
        viscous: &[[[[f64; 3]; 3]; 3]],
        step: f64,
    ) -> Result<Value, CaeError> {
        positive_step(step)?;
        self.surface.validate_levelset(levelset)?;
        let motion = self.surface.geometry().validate_motion(previous, current)?;
        let nf = self.face_count;
        let pflat: Vec<f64> = pressure.iter().flatten().copied().collect();
        real_array(&pflat, "pressure_Pa")?;
        if pressure.len() != nf {
            return Err(geometry_error(format!(
                "pressure_Pa must have shape ({nf}, 3), got ({}, 3)",
                pressure.len()
            )));
        }
        let vflat: Vec<f64> = viscous.iter().flatten().flatten().flatten().copied().collect();
        real_array(&vflat, "viscous_stress_Pa")?;
        if viscous.len() != nf {
            return Err(geometry_error(format!(
                "viscous_stress_Pa must have shape ({nf}, 3, 3, 3), got ({}, 3, 3, 3)",
                viscous.len()
            )));
        }
        let mut asymmetry = 0.0_f64;
        for face in viscous {
            for t in face {
                for i in 0..3 {
                    for j in 0..3 {
                        asymmetry = asymmetry.max((t[i][j] - t[j][i]).abs());
                    }
                }
            }
        }

        Ok(json!({"geometry": motion.to_json(), "maximum_viscous_stress_asymmetry_Pa": asymmetry,
            "impermeable_relative_mass_flux": true, "physical_qualification": false}))
    }


    pub fn add_fluid_source(
        &self,
        current_extensive: &[[f64; 5]],
        boundary_fluid_cells: &[usize],
        exchange: &InterfaceInterval<f64>,
        step: f64,
    ) -> Result<Vec<[f64; 5]>, CaeError> {
        if boundary_fluid_cells.len() != self.face_count {
            return Err(geometry_error(
                "explicit face-to-fluid-cell indices and [cell,5] conserved state required",
            ));
        }
        if boundary_fluid_cells.iter().any(|c| *c >= current_extensive.len()) {
            return Err(geometry_error("interface-to-fluid-cell index outside conserved state"));
        }
        let mut q = current_extensive.to_vec();
        for (f, cell) in boundary_fluid_cells.iter().enumerate() {
            for k in 0..5 {
                q[*cell][k] += step * exchange.fluid_conserved_source_rate[f][k];
            }
        }
        Ok(q)
    }


    #[allow(clippy::too_many_arguments)]
    pub fn sparse_partials(
        &self,
        previous: &[Point<f64>],
        current: &[Point<f64>],
        levelset: &[f64],
        pressure: &[[f64; 3]],
        viscous: &[[[[f64; 3]; 3]; 3]],
        step: f64,
    ) -> Result<ExchangePartials, CaeError> {
        self.validate(previous, current, levelset, pressure, viscous, step)?;
        let nf = self.face_count;
        let nn = self.surface.geometry().node_count();
        let rows_out = 3 * nn + 6 * nf;
        let mut triplets: [Vec<(usize, usize, f64)>; 6] = Default::default();
        for f in 0..nf {
            let x = self.local_inputs(f, previous, current, levelset, pressure, viscous, step);
            let edges = self.edge_local[f];
            let jac = implexity_ad::forward::jacobian::<8, _>(
                |s: &[Dual<8>]| exchange_local(&edges, s).to_vec(),
                &x,
            )
            .map_err(|e| geometry_error(e.to_string()))?;
            let cell = self.cells[f];
            let row_of = |o: usize| {
                if o < 12 {
                    3 * cell[o / 3] + o % 3
                } else if o < 17 {
                    3 * nn + 5 * f + (o - 12)
                } else {
                    3 * nn + 5 * nf + f
                }
            };
            let column = |block: usize, k: usize| match block {
                0 | 1 => 3 * cell[k / 3] + k % 3,
                2 => cell[k],
                3 => 3 * f + k,
                4 => 27 * f + k,
                _ => 0,
            };
            let ranges = [(0, 12), (12, 24), (24, 28), (28, 31), (31, 58), (58, 59)];
            for o in 0..18 {
                for (block, (lo, hi)) in ranges.iter().enumerate() {
                    for c in *lo..*hi {
                        triplets[block].push((row_of(o), column(block, c - lo), jac.matrix[o * 59 + c]));
                    }
                }
            }
        }
        let sizes = [3 * nn, 3 * nn, nn, 3 * nf, 27 * nf, 1];
        let [a, b, c, d, e, g] = triplets;
        Ok(ExchangePartials {
            previous_points: sparse_sum(rows_out, sizes[0], a)?,
            current_points: sparse_sum(rows_out, sizes[1], b)?,
            levelset: sparse_sum(rows_out, sizes[2], c)?,
            pressure: sparse_sum(rows_out, sizes[3], d)?,
            viscous_stress: sparse_sum(rows_out, sizes[4], e)?,
            step: sparse_sum(rows_out, sizes[5], g)?,
        })
    }

    #[must_use]
    pub fn vector(exchange: &InterfaceInterval<f64>) -> Vec<f64> {
        let mut v: Vec<f64> = exchange.solid_internal_nodal_force.iter().flatten().copied().collect();
        v.extend(exchange.fluid_conserved_source_rate.iter().flatten());
        v.extend(&exchange.internal_work);
        v
    }

    #[must_use]
    pub fn metadata() -> Value {
        exchange_metadata()
    }
}

#[derive(Debug, Clone)]
pub struct MovingMaterialBuild {
    pub geometry: Arc<TetrahedralAleGeometry>,
    pub material_surface: ImplicitMaterialSurface,
    pub fluid_cut_ale: MaterialCutAle,
    pub interface_exchange: MaterialInterfaceExchange,
    pub fluid_boundary_cells: Vec<usize>,
    pub field_solver_installed: bool,
    pub physical_qualification: bool,
}


pub fn build(
    reference_points: &[Point<f64>],
    tetrahedra: &[[i64; 4]],
    reference_levelset: &[f64],
) -> Result<MovingMaterialBuild, CaeError> {
    let geometry = Arc::new(TetrahedralAleGeometry::new(reference_points, tetrahedra)?);
    let material = ImplicitMaterialSurface::new(Arc::clone(&geometry), reference_levelset)?;
    let fluid = MaterialCutAle::new(material.clone(), Phase::Negative)?;
    let exchange = MaterialInterfaceExchange::new(material.clone())?;
    Ok(MovingMaterialBuild {
        geometry,
        fluid_boundary_cells: fluid.interface_cells().to_vec(),
        material_surface: material,
        fluid_cut_ale: fluid,
        interface_exchange: exchange,
        field_solver_installed: false,
        physical_qualification: false,
    })
}


pub fn material_support(
    material_surface: &ImplicitMaterialSurface,
    shear: &[f64],
    lame: &[f64],
    density: &[f64],
) -> Result<ImplicitHyperelasticSupport, CaeError> {
    ImplicitHyperelasticSupport::new(material_surface.clone(), shear, lame, density)
}
