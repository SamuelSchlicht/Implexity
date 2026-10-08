// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use implexity_ad::Scalar;
use implexity_core::error::CaeResult;
use implexity_linalg::sparse::CsrMatrix;
use serde_json::{Value, json};

use crate::ale_geometry::{Point, TetrahedralAleGeometry, err, real_array};
use crate::ale_transport::{AleExtensiveTransport, TransportInterval, TransportPartials};
use crate::implicit_material_surface::{EDGES, ImplicitMaterialSurface};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Positive,
    Negative,
}

impl Phase {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
        }
    }

    fn sign(self) -> f64 {
        match self {
            Self::Positive => 1.0,
            Self::Negative => -1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointPartials {
    pub parent_points_m: CsrMatrix,
    pub levelset: CsrMatrix,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CutTransportPartials {
    pub transport: TransportPartials,
    pub levelset: CsrMatrix,
}

pub type MetricPartials = BTreeMap<&'static str, BTreeMap<&'static str, CsrMatrix>>;

fn vstack(a: &CsrMatrix, b: &CsrMatrix) -> CaeResult<CsrMatrix> {
    let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
    for (offset, m) in [(0, a), (a.nrows(), b)] {
        for i in 0..m.nrows() {
            let (idx, data) = m.row(i);
            for (j, v) in idx.iter().zip(data) {
                rows.push(offset + i);
                cols.push(*j);
                vals.push(*v);
            }
        }
    }
    CsrMatrix::from_triplets(a.nrows() + b.nrows(), a.ncols(), &rows, &cols, &vals)
        .map_err(|e| err(e.to_string()))
}

fn mm(a: &CsrMatrix, b: &CsrMatrix) -> CaeResult<CsrMatrix> {
    a.matmul(b).map_err(|e| err(e.to_string()))
}

fn plus(a: &CsrMatrix, b: &CsrMatrix) -> CaeResult<CsrMatrix> {
    a.add_scaled(1.0, b, 1.0).map_err(|e| err(e.to_string()))
}

fn cut_points<S: Scalar>(
    cut: &ImplicitMaterialSurface,
    phase: Phase,
    parent_points: &[Point<S>],
    levelset: &[S],
) -> CaeResult<Vec<Point<S>>> {
    let signed: Vec<S> = levelset.iter().map(|v| *v * phase.sign()).collect();
    let mut out = parent_points.to_vec();
    out.extend(cut.trace_points(parent_points, &signed)?);
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct MaterialCutAle {
    surface: ImplicitMaterialSurface,
    phase: Phase,
    cut: ImplicitMaterialSurface,
    parent_cells: Vec<usize>,
    geometry: TetrahedralAleGeometry,
    transport: AleExtensiveTransport,
    interface_faces: Vec<usize>,
    interface_cells: Vec<usize>,
    outer_boundary_faces: Vec<usize>,
}

impl MaterialCutAle {


    pub fn new(surface: ImplicitMaterialSurface, phase: Phase) -> CaeResult<Self> {
        let parent = Arc::clone(surface.geometry());
        let cut = match phase {
            Phase::Positive => surface.clone(),
            Phase::Negative => {
                let negated: Vec<f64> = surface.reference_levelset().iter().map(|v| -v).collect();
                ImplicitMaterialSurface::new(Arc::clone(&parent), &negated)?
            }
        };
        let nc = parent.node_count();
        let edge_ids: HashMap<[usize; 2], usize> =
            cut.edges().iter().enumerate().map(|(k, e)| (*e, k)).collect();
        let (mut cells, mut parents) = (Vec::new(), Vec::new());
        for (e, pc) in parent.cells().iter().enumerate() {
            let ids: Vec<i64> = pc
                .iter()
                .map(|&i| i64::try_from(i).unwrap_or(-1))
                .chain(EDGES.iter().map(|&(i, j)| {
                    let key = [pc[i].min(pc[j]), pc[i].max(pc[j])];
                    i64::try_from(nc).unwrap_or(0)
                        + edge_ids.get(&key).map_or(-1, |&k| i64::try_from(k).unwrap_or(-1))
                }))
                .collect();
            for sub in &cut.subtets()[e] {
                cells.push(sub.map(|k| ids[k]));
                parents.push(e);
            }
        }
        if cells.is_empty() {
            return Err(err(
                "selected material phase is absent: allocate no phase state, rather than a ghost control volume",
            ));
        }
        let points = cut_points(&cut, phase, parent.reference_points_m(), surface.reference_levelset())?;
        let geometry = TetrahedralAleGeometry::new(&points, &cells)?;
        let lookup: HashMap<[usize; 3], usize> = geometry
            .boundary_faces()
            .iter()
            .map(|&f| {
                let mut k = geometry.faces()[f];
                k.sort_unstable();
                (k, f)
            })
            .collect();
        let mut interface_faces = Vec::with_capacity(surface.triangles().len());
        for t in surface.triangles() {
            let mut k = t.map(|v| nc + v);
            k.sort_unstable();
            interface_faces
                .push(*lookup.get(&k).ok_or_else(|| err("nonconforming material interface triangulation"))?);
        }
        let interface_cells = interface_faces.iter().map(|&f| geometry.left()[f]).collect();
        let outer_boundary_faces =
            geometry.boundary_faces().iter().copied().filter(|f| !interface_faces.contains(f)).collect();
        Ok(Self {
            surface,
            phase,
            cut,
            parent_cells: parents,
            transport: AleExtensiveTransport::new(geometry.clone()),
            geometry,
            interface_faces,
            interface_cells,
            outer_boundary_faces,
        })
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub fn surface(&self) -> &ImplicitMaterialSurface {
        &self.surface
    }

    #[must_use]
    pub fn geometry(&self) -> &TetrahedralAleGeometry {
        &self.geometry
    }

    #[must_use]
    pub fn transport(&self) -> &AleExtensiveTransport {
        &self.transport
    }

    #[must_use]
    pub fn parent_cells(&self) -> &[usize] {
        &self.parent_cells
    }

    #[must_use]
    pub fn interface_faces(&self) -> &[usize] {
        &self.interface_faces
    }

    #[must_use]
    pub fn interface_cells(&self) -> &[usize] {
        &self.interface_cells
    }

    #[must_use]
    pub fn outer_boundary_faces(&self) -> &[usize] {
        &self.outer_boundary_faces
    }

    fn signed<S: Scalar>(&self, levelset: &[S]) -> Vec<S> {
        levelset.iter().map(|v| *v * self.phase.sign()).collect()
    }



    pub fn points<S: Scalar>(&self, parent_points: &[Point<S>], levelset: &[S]) -> CaeResult<Vec<Point<S>>> {
        cut_points(&self.cut, self.phase, parent_points, levelset)
    }



    pub fn point_partials(&self, parent_points: &[Point<f64>], levelset: &[f64]) -> CaeResult<PointPartials> {
        let nn = self.surface.geometry().node_count();
        real_array(&parent_points.concat(), "parent_points_m")?;
        if parent_points.len() != nn {
            return Err(err(format!(
                "parent_points_m must have shape ({nn}, 3), got ({}, 3)",
                parent_points.len()
            )));
        }
        self.surface.validate_levelset(levelset)?;
        let (h, dpsi) = self.cut.trace_partials(&parent_points.concat(), 3, &self.signed(levelset))?;
        let zero = CsrMatrix::from_triplets(3 * nn, nn, &[], &[], &[]).map_err(|e| err(e.to_string()))?;
        let mut signed = dpsi;
        for v in signed.data_mut() {
            *v *= self.phase.sign();
        }
        Ok(PointPartials {
            parent_points_m: vstack(&CsrMatrix::identity(3 * nn), &h)?,
            levelset: vstack(&zero, &signed)?,
        })
    }



    pub fn metrics<S: Scalar>(
        &self,
        previous: &[Point<S>],
        current: &[Point<S>],
        levelset: &[S],
    ) -> CaeResult<crate::ale_geometry::GeometryMetrics<S>> {
        self.geometry.metrics(&self.points(previous, levelset)?, &self.points(current, levelset)?)
    }



    pub fn validate_motion(
        &self,
        previous: &[Point<f64>],
        current: &[Point<f64>],
        levelset: &[f64],
    ) -> CaeResult<Value> {
        self.surface.validate_levelset(levelset)?;
        let receipt = self
            .geometry
            .validate_motion(&self.points(previous, levelset)?, &self.points(current, levelset)?)?;
        let mut out = receipt.to_json();
        if let Value::Object(m) = &mut out {
            m.insert("phase".into(), json!(self.phase.name()));
            m.insert("actual_phase_volumes".into(), json!(true));
            m.insert("reference_domain_mass_compensation".into(), json!(false));
            m.insert("topology_rebuilt".into(), json!(false));
            m.insert("history_remapped".into(), json!(false));
        }
        Ok(out)
    }



    pub fn metric_partials(
        &self,
        previous: &[Point<f64>],
        current: &[Point<f64>],
        levelset: &[f64],
    ) -> CaeResult<MetricPartials> {
        self.validate_motion(previous, current, levelset)?;
        let x0 = self.points(previous, levelset)?;
        let x1 = self.points(current, levelset)?;
        let p0 = self.point_partials(previous, levelset)?;
        let p1 = self.point_partials(current, levelset)?;
        let raw = self.geometry.sparse_partials(&x0, &x1)?;
        let mut out: MetricPartials = BTreeMap::new();
        let (a, b) = (&raw.previous_points_m, &raw.current_points_m);
        for (metric, ma, mb) in [
            ("area_average_m2", &a.area_average_m2, &b.area_average_m2),
            ("swept_volume_m3", &a.swept_volume_m3, &b.swept_volume_m3),
        ] {
            out.entry("previous_points_m").or_default().insert(metric, mm(ma, &p0.parent_points_m)?);
            out.entry("current_points_m").or_default().insert(metric, mm(mb, &p1.parent_points_m)?);
            out.entry("levelset")
                .or_default()
                .insert(metric, plus(&mm(ma, &p0.levelset)?, &mm(mb, &p1.levelset)?)?);
        }
        for (metric, key, m, p) in [
            ("volume_start_m3", "previous_points_m", &raw.volume_start_m3, &p0),
            ("volume_end_m3", "current_points_m", &raw.volume_end_m3, &p1),
        ] {
            out.entry(key).or_default().insert(metric, mm(m, &p.parent_points_m)?);
            out.entry("levelset").or_default().insert(metric, mm(m, &p.levelset)?);
        }
        Ok(out)
    }



    #[allow(clippy::too_many_arguments)]
    pub fn interval<S: Scalar>(
        &self,
        previous_extensive: &[S],
        previous_points: &[Point<S>],
        current_points: &[Point<S>],
        levelset: &[S],
        face_velocity: &[Point<S>],
        exterior_intensive: &[S],
        step: S,
    ) -> CaeResult<TransportInterval<S>> {
        self.transport.interval(
            previous_extensive,
            &self.points(previous_points, levelset)?,
            &self.points(current_points, levelset)?,
            face_velocity,
            exterior_intensive,
            step,
        )
    }



    #[allow(clippy::too_many_arguments)]
    pub fn residual<S: Scalar>(
        &self,
        current_extensive: &[S],
        previous_extensive: &[S],
        previous_points: &[Point<S>],
        current_points: &[Point<S>],
        levelset: &[S],
        face_velocity: &[Point<S>],
        exterior_intensive: &[S],
        step: S,
    ) -> CaeResult<Vec<S>> {
        let next = self.interval(
            previous_extensive,
            previous_points,
            current_points,
            levelset,
            face_velocity,
            exterior_intensive,
            step,
        )?;
        Ok(current_extensive.iter().zip(&next.current_extensive).map(|(a, b)| *a - *b).collect())
    }



    #[allow(clippy::too_many_arguments)]
    pub fn sparse_partials(
        &self,
        previous_extensive: &[f64],
        previous_points: &[Point<f64>],
        current_points: &[Point<f64>],
        levelset: &[f64],
        face_velocity: &[Point<f64>],
        exterior_intensive: &[f64],
        step: f64,
    ) -> CaeResult<CutTransportPartials> {
        self.validate_motion(previous_points, current_points, levelset)?;
        let p0 = self.point_partials(previous_points, levelset)?;
        let p1 = self.point_partials(current_points, levelset)?;
        let mut raw = self.transport.sparse_partials(
            previous_extensive,
            &self.points(previous_points, levelset)?,
            &self.points(current_points, levelset)?,
            face_velocity,
            exterior_intensive,
            step,
        )?;
        let (a, b) = (raw.previous_points_m.clone(), raw.current_points_m.clone());
        raw.previous_points_m = mm(&a, &p0.parent_points_m)?;
        raw.current_points_m = mm(&b, &p1.parent_points_m)?;
        let levelset_block = plus(&mm(&a, &p0.levelset)?, &mm(&b, &p1.levelset)?)?;
        Ok(CutTransportPartials { transport: raw, levelset: levelset_block })
    }

    #[must_use]
    pub fn metadata(&self) -> Value {
        json!({"schema": "implexity-material-cut-ale/1", "phase": self.phase.name(),
            "physical_phase_volumes": true, "triangulation": "globally_ordered_pulling_shared_face_diagonals",
            "interface_facets": self.interface_faces.len(), "control_volumes": self.geometry.cell_count(),
            "material_shape_and_both_time_geometry_partials": true, "reference_domain_mass_compensation": false,
            "conservative_topology_change_remap": false, "contact_checked": false,
            "finite_motion_fsi_field_solver": false, "physical_qualification": false})
    }
}

