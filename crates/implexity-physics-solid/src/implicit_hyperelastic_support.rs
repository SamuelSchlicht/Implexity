// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::ale_geometry::{Point, real_array};
use implexity_solve::implicit_material_surface::{ImplicitMaterialSurface, fraction, subtet_barycentric};

use crate::hyperelastic::kinematics::{Mat3, TetMesh, det, neo_hookean_energy, neo_hookean_piola};
use crate::moving_interface::sparse_sum;

fn geometry_error(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

fn cell_array(values: &[f64], name: &str, count: usize) -> Result<Vec<f64>, CaeError> {
    real_array(values, name)?;
    if values.len() != count {
        return Err(geometry_error(format!("{name} must have shape ({count},), got ({},)", values.len())));
    }
    Ok(values.to_vec())
}

#[derive(Debug, Clone)]
pub struct ImplicitHyperelasticSupport {
    surface: ImplicitMaterialSurface,
    pub shear: Vec<f64>,
    pub lame: Vec<f64>,
    pub density: Vec<f64>,
    active_mesh: TetMesh,
}

fn local_u<S: Scalar>(u: &[Point<S>], cell: &[usize; 4]) -> [[S; 3]; 4] {
    cell.map(|n| u[n])
}

fn local_inertia<S: Scalar>(
    acceleration: &[[S; 3]; 4],
    levelset: &[S; 4],
    reference: &[[f64; 3]; 4],
    crossing: [bool; 6],
    subtets: &[[usize; 4]],
    density: f64,
) -> [[S; 3]; 4] {
    let bary = subtet_barycentric(levelset, &crossing, subtets);
    let edges: Mat3<f64> =
        std::array::from_fn(|r| std::array::from_fn(|c| reference[r + 1][c] - reference[0][c]));
    let reference_volume = det(&edges) / 6.0;
    let mut integral = [[S::zero(); 4]; 4];
    for b in &bary {
        let points: Mat3<S> = std::array::from_fn(|r| std::array::from_fn(|c| b[r + 1][c + 1] - b[0][c + 1]));
        let volume = det(&points) * reference_volume;
        let sums: [S; 4] = std::array::from_fn(|i| b[0][i] + b[1][i] + b[2][i] + b[3][i]);
        for i in 0..4 {
            for j in 0..4 {
                let mut outer = S::zero();
                for row in b {
                    outer += row[i] * row[j];
                }
                integral[i][j] += volume * (sums[i] * sums[j] + outer) / 20.0;
            }
        }
    }
    std::array::from_fn(|i| {
        std::array::from_fn(|k| {
            let mut acc = S::zero();
            for (j, a) in acceleration.iter().enumerate() {
                acc += integral[i][j] * a[k];
            }
            acc * density
        })
    })
}

impl ImplicitHyperelasticSupport {

    pub fn new(
        surface: ImplicitMaterialSurface,
        shear: &[f64],
        lame: &[f64],
        density: &[f64],
    ) -> Result<Self, CaeError> {
        let geometry = surface.geometry();
        let nc = geometry.cell_count();
        let shear = cell_array(shear, "shear_Pa", nc)?;
        let lame = cell_array(lame, "lame_Pa", nc)?;
        let density = cell_array(density, "density_kg_m3", nc)?;

        let active = surface.active_cells();
        if active.iter().any(|c| shear[*c] <= 0.0 || lame[*c] < 0.0 || density[*c] <= 0.0) {
            return Err(geometry_error(
                "active material requires positive shear/density and nonnegative Lame lambda",
            ));
        }
        let active_mesh = TetMesh::new(
            geometry.reference_points_m().to_vec(),
            active.iter().map(|c| geometry.cells()[*c]).collect(),
        )?;
        Ok(Self { surface, shear, lame, density, active_mesh })
    }

    fn active(&self) -> &[usize] {
        self.surface.active_cells()
    }

    fn cell(&self, c: usize) -> [usize; 4] {
        self.surface.geometry().cells()[c]
    }

    pub fn energy<S: Scalar>(&self, displacement: &[Point<S>], levelset: &[S]) -> S {
        let fractions = self.surface.volume_fractions(levelset);
        let mut total = S::zero();
        for (k, c) in self.active().iter().enumerate() {
            let f = self.active_mesh.deformation_gradient(k, &local_u(displacement, &self.cell(*c)));
            let (mu, lam) = (fractions[*c] * self.shear[*c], fractions[*c] * self.lame[*c]);
            total += neo_hookean_energy(&f, mu, lam) * self.active_mesh.volumes[k];
        }
        total
    }

    fn element_force<S: Scalar>(&self, k: usize, u: &[[S; 3]; 4], fraction: S, c: usize) -> [[S; 3]; 4] {
        let f = self.active_mesh.deformation_gradient(k, u);
        let piola = neo_hookean_piola(&f, fraction * self.shear[c], fraction * self.lame[c]);
        self.active_mesh.nodal_forces(k, &piola)
    }

    pub fn nodal_internal_force<S: Scalar>(
        &self,
        displacement: &[Point<S>],
        levelset: &[S],
    ) -> Vec<Point<S>> {
        let fractions = self.surface.volume_fractions(levelset);
        let mut out = vec![[S::zero(); 3]; displacement.len()];
        for (k, c) in self.active().iter().enumerate() {
            let cell = self.cell(*c);
            let forces = self.element_force(k, &local_u(displacement, &cell), fractions[*c], *c);
            for (a, n) in cell.iter().enumerate() {
                for i in 0..3 {
                    out[*n][i] += forces[a][i];
                }
            }
        }
        out
    }


    pub fn mass_matrix(&self, levelset: &[f64], vector: bool) -> Result<CsrMatrix, CaeError> {
        self.surface.validate_levelset(levelset)?;
        let integral = self.surface.material_shape_integrals(levelset).second_m3;
        let nn = self.surface.geometry().node_count();
        let mut triplets = Vec::new();
        for c in self.active() {
            let cell = self.cell(*c);
            for a in 0..4 {
                for b in 0..4 {
                    let v = integral[*c][a][b] * self.density[*c];
                    if vector {
                        for k in 0..3 {
                            triplets.push((3 * cell[a] + k, 3 * cell[b] + k, v));
                        }
                    } else {
                        triplets.push((cell[a], cell[b], v));
                    }
                }
            }
        }
        let size = if vector { 3 * nn } else { nn };
        sparse_sum(size, size, triplets)
    }

    fn inertia_inputs<S: Scalar>(
        &self,
        c: usize,
        acceleration: &[Point<S>],
        levelset: &[S],
    ) -> ([[S; 3]; 4], [S; 4], [[f64; 3]; 4]) {
        let cell = self.cell(c);
        let points = self.surface.geometry().reference_points_m();
        (local_u(acceleration, &cell), cell.map(|n| levelset[n]), cell.map(|n| points[n]))
    }


    pub fn nodal_inertial_force<S: Scalar>(
        &self,
        acceleration: &[Point<S>],
        levelset: &[S],
    ) -> Result<Vec<Point<S>>, CaeError> {
        let nn = self.surface.geometry().node_count();
        if acceleration.len() != nn {
            return Err(geometry_error("acceleration_m_s2 must have shape [node,3]"));
        }
        let mut out = vec![[S::zero(); 3]; nn];
        for c in self.active() {
            let (a, psi, x) = self.inertia_inputs(*c, acceleration, levelset);
            let local = local_inertia(
                &a,
                &psi,
                &x,
                self.surface.crossing()[*c],
                &self.surface.subtets()[*c],
                self.density[*c],
            );
            for (k, n) in self.cell(*c).iter().enumerate() {
                for i in 0..3 {
                    out[*n][i] += local[k][i];
                }
            }
        }
        Ok(out)
    }


    pub fn kinetic_energy<S: Scalar>(&self, velocity: &[Point<S>], levelset: &[S]) -> Result<S, CaeError> {
        let f = self.nodal_inertial_force(velocity, levelset)?;
        let mut acc = S::zero();
        for (v, g) in velocity.iter().zip(&f) {
            for i in 0..3 {
                acc += v[i] * g[i];
            }
        }
        Ok(acc * 0.5)
    }


    pub fn inertia_sparse_partials(
        &self,
        acceleration: &[Point<f64>],
        levelset: &[f64],
    ) -> Result<(CsrMatrix, CsrMatrix), CaeError> {
        self.surface.validate_levelset(levelset)?;
        let nn = self.surface.geometry().node_count();
        let flat: Vec<f64> = acceleration.iter().flatten().copied().collect();
        real_array(&flat, "acceleration_m_s2")?;
        if acceleration.len() != nn {
            return Err(geometry_error(format!(
                "acceleration_m_s2 must have shape ({nn}, 3), got ({}, 3)",
                acceleration.len()
            )));
        }
        let (mut ta, mut tl) = (Vec::new(), Vec::new());
        for c in self.active() {
            let (a, psi, x) = self.inertia_inputs(*c, acceleration, levelset);
            let cell = self.cell(*c);
            let crossing = self.surface.crossing()[*c];
            let subtets = &self.surface.subtets()[*c];
            let input: Vec<f64> = a.iter().flatten().copied().chain(psi).collect();
            let jac = implexity_ad::forward::jacobian::<16, _>(
                |s: &[Dual<16>]| {
                    let acc: [[Dual<16>; 3]; 4] =
                        std::array::from_fn(|k| std::array::from_fn(|i| s[3 * k + i]));
                    let ps: [Dual<16>; 4] = std::array::from_fn(|k| s[12 + k]);
                    local_inertia(&acc, &ps, &x, crossing, subtets, self.density[*c])
                        .iter()
                        .flatten()
                        .copied()
                        .collect()
                },
                &input,
            )
            .map_err(|e| geometry_error(e.to_string()))?;
            for o in 0..12 {
                let row = 3 * cell[o / 3] + o % 3;
                for k in 0..12 {
                    ta.push((row, 3 * cell[k / 3] + k % 3, jac.matrix[o * 16 + k]));
                }
                for k in 0..4 {
                    tl.push((row, cell[k], jac.matrix[o * 16 + 12 + k]));
                }
            }
        }
        Ok((sparse_sum(3 * nn, 3 * nn, ta)?, sparse_sum(3 * nn, nn, tl)?))
    }


    pub fn validate_state(&self, displacement: &[Point<f64>], levelset: &[f64]) -> Result<Value, CaeError> {
        self.surface.validate_levelset(levelset)?;
        let nn = self.surface.geometry().node_count();
        let flat: Vec<f64> = displacement.iter().flatten().copied().collect();
        real_array(&flat, "displacement_m")?;
        if displacement.len() != nn {
            return Err(geometry_error(format!(
                "displacement_m must have shape ({nn}, 3), got ({}, 3)",
                displacement.len()
            )));
        }
        let j: Vec<f64> = (0..self.active().len())
            .map(|k| {
                det(&self
                    .active_mesh
                    .deformation_gradient(k, &local_u(displacement, &self.cell(self.active()[k]))))
            })
            .collect();
        if j.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(geometry_error("nonpositive deformation determinant in actual material"));
        }
        let fractions = self.surface.volume_fractions(levelset);
        if self.active().iter().any(|c| fractions[*c] <= 0.0)
            || fractions.iter().any(|f| *f > 1.0 + 64.0 * f64::EPSILON)
        {
            return Err(geometry_error("invalid cut volume, rebuild/relinearize the local trial"));
        }
        let min = |v: Vec<f64>| {
            if v.is_empty() { Value::Null } else { json!(v.iter().copied().fold(f64::INFINITY, f64::min)) }
        };
        Ok(json!({"minimum_material_J": min(j),
            "minimum_positive_reference_fraction": min(self.active().iter().map(|c| fractions[*c]).collect()),
            "unsupported_nodes": self.surface.unsupported_nodes(), "ghost_support_installed": false,
            "pressure_contact_or_material_calibration_qualified": false}))
    }


    pub fn sparse_partials(
        &self,
        displacement: &[Point<f64>],
        levelset: &[f64],
    ) -> Result<(CsrMatrix, CsrMatrix), CaeError> {
        self.validate_state(displacement, levelset)?;
        let nn = self.surface.geometry().node_count();
        let (mut tu, mut tl) = (Vec::new(), Vec::new());
        for (k, c) in self.active().iter().enumerate() {
            let cell = self.cell(*c);
            let crossing = self.surface.crossing()[*c];
            let subtets = &self.surface.subtets()[*c];
            let input: Vec<f64> = local_u(displacement, &cell)
                .iter()
                .flatten()
                .copied()
                .chain(cell.map(|n| levelset[n]))
                .collect();
            let jac = implexity_ad::forward::jacobian::<16, _>(
                |s: &[Dual<16>]| {
                    let u: [[Dual<16>; 3]; 4] =
                        std::array::from_fn(|a| std::array::from_fn(|i| s[3 * a + i]));
                    let psi: [Dual<16>; 4] = std::array::from_fn(|a| s[12 + a]);
                    let fr = fraction(&psi, &crossing, subtets);
                    self.element_force(k, &u, fr, *c).iter().flatten().copied().collect()
                },
                &input,
            )
            .map_err(|e| geometry_error(e.to_string()))?;
            for o in 0..12 {
                let row = 3 * cell[o / 3] + o % 3;
                for q in 0..12 {
                    tu.push((row, 3 * cell[q / 3] + q % 3, jac.matrix[o * 16 + q]));
                }
                for q in 0..4 {
                    tl.push((row, cell[q], jac.matrix[o * 16 + 12 + q]));
                }
            }
        }
        Ok((sparse_sum(3 * nn, 3 * nn, tu)?, sparse_sum(3 * nn, nn, tl)?))
    }

    #[must_use]
    pub fn metadata() -> Value {
        json!({"schema": "implexity-cut-hyperelastic-support/1",
            "constitutive_source": "existing_hyperelasticity.stored_energy_compressible_neo_hookean",
            "strain": "finite", "integration": "exact_linear_tetra_material_cut", "mass": "consistent_exact_cut_moments",
            "inactive_material": "absent_no_ersatz", "shape_partials": true,
            "calibrations_supplied": false, "small_strain_native_owner_replaced": false,
            "dynamics_contact_or_fluid_solve_included": false, "physical_qualification": false})
    }
}
