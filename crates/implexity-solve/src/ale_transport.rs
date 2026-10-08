// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::error::CaeResult;
use implexity_linalg::sparse::CsrMatrix;
use serde_json::{Value, json};

use crate::ale_geometry::{GeometryMetrics, Point, TetrahedralAleGeometry, err, positive_step, real_array};
use crate::matrix::eliminate_zeros;

#[derive(Clone, Debug, PartialEq)]
pub struct TransportInterval<S> {
    pub current_extensive: Vec<S>,
    pub current_intensive: Vec<S>,
    pub integrated_outward_flux: Vec<S>,
    pub relative_volume_flux_m3: Vec<S>,
    pub outgoing_volume_fraction: Vec<S>,
    pub metrics: GeometryMetrics<S>,
    pub conservation_error: Vec<S>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransportPartials {
    pub current_extensive: CsrMatrix,
    pub previous_extensive: CsrMatrix,
    pub face_velocity_m_s: CsrMatrix,
    pub exterior_intensive: CsrMatrix,
    pub step_s: CsrMatrix,
    pub previous_points_m: CsrMatrix,
    pub current_points_m: CsrMatrix,
}

#[derive(Clone, Debug)]
pub struct AleExtensiveTransport {
    geometry: TetrahedralAleGeometry,
}

fn triplets(nrows: usize, ncols: usize, t: &[(usize, usize, f64)]) -> CaeResult<CsrMatrix> {
    let rows: Vec<usize> = t.iter().map(|e| e.0).collect();
    let cols: Vec<usize> = t.iter().map(|e| e.1).collect();
    let vals: Vec<f64> = t.iter().map(|e| e.2).collect();
    CsrMatrix::from_triplets(nrows, ncols, &rows, &cols, &vals).map_err(|e| err(e.to_string()))
}

fn kron_identity(a: &CsrMatrix, k: usize) -> CaeResult<CsrMatrix> {
    let mut t = Vec::with_capacity(a.nnz() * k);
    for i in 0..a.nrows() {
        let (idx, data) = a.row(i);
        for c in 0..k {
            for (j, v) in idx.iter().zip(data) {
                t.push((i * k + c, j * k + c, *v));
            }
        }
    }
    triplets(a.nrows() * k, a.ncols() * k, &t)
}

fn mm(a: &CsrMatrix, b: &CsrMatrix) -> CaeResult<CsrMatrix> {
    a.matmul(b).map_err(|e| err(e.to_string()))
}

fn add(a: &CsrMatrix, alpha: f64, b: &CsrMatrix, beta: f64) -> CaeResult<CsrMatrix> {
    a.add_scaled(alpha, b, beta).map_err(|e| err(e.to_string()))
}

impl AleExtensiveTransport {
    #[must_use]
    pub fn new(geometry: TetrahedralAleGeometry) -> Self {
        Self { geometry }
    }

    #[must_use]
    pub fn geometry(&self) -> &TetrahedralAleGeometry {
        &self.geometry
    }

    fn components(&self, q_len: usize) -> CaeResult<usize> {
        let nc = self.geometry.cell_count();
        if q_len == 0 || !q_len.is_multiple_of(nc) {
            return Err(err("previous_extensive requires [cell,component]"));
        }
        Ok(q_len / nc)
    }



    #[allow(clippy::too_many_arguments)]
    pub fn interval<S: Scalar>(
        &self,
        previous_extensive: &[S],
        previous_points: &[Point<S>],
        current_points: &[Point<S>],
        face_velocity: &[Point<S>],
        exterior_intensive: &[S],
        step: S,
    ) -> CaeResult<TransportInterval<S>> {
        let g = &self.geometry;
        let k = self.components(previous_extensive.len())?;
        let (nc, nf) = (g.cell_count(), g.face_count());
        if face_velocity.len() != nf || exterior_intensive.len() != nf * k {
            return Err(err("face velocity/exterior intensive shapes do not match"));
        }
        let m = g.metrics(previous_points, current_points)?;
        let q = previous_extensive;
        let density = |cell: usize, c: usize| q[cell * k + c] / m.volume_start_m3[cell];
        let volume_flux: Vec<S> = m
            .faces
            .iter()
            .zip(face_velocity)
            .map(|(f, v)| {
                let a = f.area_average_m2;
                step * (a[0] * v[0] + a[1] * v[1] + a[2] * v[2]) - f.swept_volume_m3
            })
            .collect();
        let mut flux = Vec::with_capacity(nf * k);
        for f in 0..nf {
            let upwind = volume_flux[f].value() >= 0.0;
            for c in 0..k {
                let donor = if upwind {
                    density(g.left()[f], c)
                } else {
                    match g.right()[f] {
                        Some(r) => density(r, c),
                        None => exterior_intensive[f * k + c],
                    }
                };
                flux.push(volume_flux[f] * donor);
            }
        }
        let div = g.divergence(&flux, k)?;
        let updated: Vec<S> = q.iter().zip(&div).map(|(a, d)| *a - *d).collect();
        let mut debit = vec![S::zero(); nc];
        for (&l, v) in g.left().iter().zip(&volume_flux) {
            debit[l] += v.max_f64(0.0);
        }
        for &f in g.interior_faces() {
            if let Some(r) = g.right()[f] {
                debit[r] += (-volume_flux[f]).max_f64(0.0);
            }
        }
        let current_intensive = (0..nc * k).map(|i| updated[i] / m.volume_end_m3[i / k]).collect();
        let outgoing = debit.iter().zip(&m.volume_start_m3).map(|(d, v)| *d / *v).collect();
        let conservation = (0..k)
            .map(|c| {
                let change =
                    (0..nc).fold(S::zero(), |acc, cell| acc + (updated[cell * k + c] - q[cell * k + c]));
                g.boundary_faces().iter().fold(change, |acc, &f| acc + flux[f * k + c])
            })
            .collect();
        Ok(TransportInterval {
            current_extensive: updated,
            current_intensive,
            integrated_outward_flux: flux,
            relative_volume_flux_m3: volume_flux,
            outgoing_volume_fraction: outgoing,
            metrics: m,
            conservation_error: conservation,
        })
    }



    #[allow(clippy::too_many_arguments)]
    pub fn residual<S: Scalar>(
        &self,
        current_extensive: &[S],
        previous_extensive: &[S],
        previous_points: &[Point<S>],
        current_points: &[Point<S>],
        face_velocity: &[Point<S>],
        exterior_intensive: &[S],
        step: S,
    ) -> CaeResult<Vec<S>> {
        let next = self.interval(
            previous_extensive,
            previous_points,
            current_points,
            face_velocity,
            exterior_intensive,
            step,
        )?;
        if current_extensive.len() != next.current_extensive.len() {
            return Err(err("previous_extensive requires [cell,component]"));
        }
        Ok(current_extensive.iter().zip(&next.current_extensive).map(|(a, b)| *a - *b).collect())
    }



    #[allow(clippy::too_many_arguments)]
    pub fn check(
        &self,
        previous_extensive: &[f64],
        previous_points: &[Point<f64>],
        current_points: &[Point<f64>],
        face_velocity: &[Point<f64>],
        exterior_intensive: &[f64],
        step: f64,
        nonnegative_components: &[usize],
    ) -> CaeResult<Value> {
        let step = positive_step(step)?;
        real_array(previous_extensive, "previous_extensive")?;
        let k = self.components(previous_extensive.len())?;
        real_array(&face_velocity.concat(), "face_velocity_m_s")?;
        if face_velocity.len() != self.geometry.face_count() {
            return Err(err(format!(
                "face_velocity_m_s must have shape ({}, 3), got ({}, 3)",
                self.geometry.face_count(),
                face_velocity.len()
            )));
        }
        real_array(exterior_intensive, "exterior_intensive")?;
        let mut sorted = nonnegative_components.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() != nonnegative_components.len() || nonnegative_components.iter().any(|&c| c >= k) {
            return Err(err("nonnegative component indices must be distinct and in range"));
        }
        let motion = self.geometry.validate_motion(previous_points, current_points)?;
        let out = self.interval(
            previous_extensive,
            previous_points,
            current_points,
            face_velocity,
            exterior_intensive,
            step,
        )?;
        if !out.current_extensive.iter().chain(&out.integrated_outward_flux).all(Scalar::is_finite) {
            return Err(err("nonfinite extensive transport"));
        }
        let max_outgoing = out.outgoing_volume_fraction.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let negatives = (0..self.geometry.cell_count())
            .flat_map(|cell| nonnegative_components.iter().map(move |&c| cell * k + c))
            .filter(|&i| out.current_extensive[i] < 0.0)
            .count();
        Ok(json!({
            "geometry": motion.to_json(),
            "maximum_outgoing_volume_fraction": max_outgoing,
            "monotone_outflow_condition": max_outgoing <= 1.0,
            "negative_authored_nonnegative_entries": negatives,
            "maximum_abs_conservation_error": out.conservation_error.iter().map(|v| v.abs()).fold(f64::NEG_INFINITY, f64::max),
            "clipping_performed": false, "engineering_acceptance_performed": false,
            "global_fluid_or_total_energy_model": false,
        }))
    }



    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn sparse_partials(
        &self,
        previous_extensive: &[f64],
        previous_points: &[Point<f64>],
        current_points: &[Point<f64>],
        face_velocity: &[Point<f64>],
        exterior_intensive: &[f64],
        step: f64,
    ) -> CaeResult<TransportPartials> {
        let g = &self.geometry;
        let dt = positive_step(step)?;
        real_array(previous_extensive, "previous_extensive")?;
        real_array(&face_velocity.concat(), "face_velocity_m_s")?;
        let out = self.interval(
            previous_extensive,
            previous_points,
            current_points,
            face_velocity,
            exterior_intensive,
            dt,
        )?;
        real_array(exterior_intensive, "exterior_intensive")?;
        let k = self.components(previous_extensive.len())?;
        let (nc, nf) = (g.cell_count(), g.face_count());
        let q = previous_extensive;
        let fvol = &out.relative_volume_flux_m3;
        let volume = &out.metrics.volume_start_m3;
        let area = out.metrics.area_average_m2();
        let selected: Vec<Option<usize>> =
            (0..nf).map(|f| if fvol[f] >= 0.0 { Some(g.left()[f]) } else { g.right()[f] }).collect();
        let donor: Vec<f64> = (0..nf * k)
            .map(|i| {
                let (f, c) = (i / k, i % k);
                selected[f].map_or(exterior_intensive[i], |s| q[s * k + c] / volume[s])
            })
            .collect();
        let density_to_flux = triplets(
            nf,
            nc,
            &(0..nf).filter_map(|f| selected[f].map(|s| (f, s, fvol[f] / volume[s]))).collect::<Vec<_>>(),
        )?;
        let exterior_to_flux = CsrMatrix::diagonal_matrix(
            &(0..nf).map(|f| if selected[f].is_some() { 0.0 } else { fvol[f] }).collect::<Vec<_>>(),
        );
        let d = kron_identity(g.incidence(), k)?;
        let w = triplets(nf * k, nf, &(0..nf * k).map(|i| (i, i / k, donor[i])).collect::<Vec<_>>())?;
        let mut vb = Vec::new();
        for f in 0..nf {
            if let Some(s) = selected[f] {
                for c in 0..k {
                    vb.push((k * f + c, s, -fvol[f] * donor[f * k + c] / volume[s]));
                }
            }
        }
        let volume_block = triplets(nf * k, nc, &vb)?;
        let p = g.sparse_partials(previous_points, current_points)?;
        let av = triplets(
            nf,
            3 * nf,
            &(0..3 * nf).map(|i| (i / 3, i, face_velocity[i / 3][i % 3])).collect::<Vec<_>>(),
        )?;
        let velocities = triplets(
            nf,
            3 * nf,
            &(0..3 * nf).map(|i| (i / 3, i, dt * area[i / 3][i % 3])).collect::<Vec<_>>(),
        )?;
        let identity = CsrMatrix::identity(nc * k);
        let previous = add(&mm(&d, &kron_identity(&density_to_flux, k)?)?, 1.0, &identity, -1.0)?;
        let dw = mm(&d, &w)?;
        let step_column: Vec<f64> = {
            let s: Vec<f64> =
                (0..nf).map(|f| (0..3).map(|j| area[f][j] * face_velocity[f][j]).sum()).collect();
            dw.matvec(&s).map_err(|e| err(e.to_string()))?
        };
        let step_matrix = triplets(
            nc * k,
            1,
            &step_column.iter().enumerate().map(|(i, v)| (i, 0, *v)).collect::<Vec<_>>(),
        )?;
        let points = |partials: &crate::ale_geometry::EndpointPartials,
                      previous_volume: bool|
         -> CaeResult<CsrMatrix> {
            let moving = add(&mm(&av, &partials.area_average_m2)?, dt, &partials.swept_volume_m3, -1.0)?;
            let mut block = mm(&w, &moving)?;
            if previous_volume {
                block = add(&block, 1.0, &mm(&volume_block, &p.volume_start_m3)?, 1.0)?;
            }
            mm(&d, &block)
        };
        let clean = |m: CsrMatrix| eliminate_zeros(&m);
        Ok(TransportPartials {
            current_extensive: identity.clone(),
            previous_extensive: clean(previous),
            face_velocity_m_s: clean(mm(&dw, &velocities)?),
            exterior_intensive: clean(mm(&d, &kron_identity(&exterior_to_flux, k)?)?),
            step_s: clean(step_matrix),
            previous_points_m: clean(points(&p.previous_points_m, true)?),
            current_points_m: clean(points(&p.current_points_m, false)?),
        })
    }

    #[must_use]
    pub fn metadata(&self) -> Value {
        json!({"schema": "implexity-ale-extensive-transport/1", "conservation": "one_oriented_shared_face_flux",
            "storage": "extensive", "donors": "previous_intensive_upwind", "motion": "exact_straight_path_swept_volumes",
            "sparse_geometry_partials": true, "topology_remap": false, "equation_of_state": false,
            "pressure_and_viscous_fluxes_included": false, "physical_qualification": false})
    }
}

