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
    complementarity::{OriginBranch, residual_partials},
    contact_field::{ContactContribution, NativeContactLaw},
};
use crate::reference_traction::ReferenceTractionOwnership;
use crate::surface_gap::{GapCellGeometry, SurfaceGapKinematics, pressure_loads};
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{matrix::Jacobian, time_stepper::StepParameters};
use std::collections::BTreeSet;
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn add<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] + b[i])
}
fn scale<S: Scalar>(a: [S; 3], b: S) -> [S; 3] {
    a.map(|a| a * b)
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn matrix(rows: usize, cols: usize, entries: &[(usize, usize, f64)]) -> CaeResult<Jacobian> {
    let mut r = vec![];
    let mut c = vec![];
    let mut v = vec![];
    for &(i, j, x) in entries {
        if !x.is_finite() {
            return Err(fail("patch contact Jacobian overflow"));
        }
        if x != 0. {
            r.push(i);
            c.push(j);
            v.push(x);
        }
    }
    Ok(Jacobian::Csr(
        CsrMatrix::from_triplets(rows, cols, &r, &c, &v)
            .map_err(|e| CaeError::contract(e.to_string()))?,
    ))
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchContactSite {
    pub cell: usize,
    pub corner: usize,
}

pub struct PatchUnilateralLaw {
    kinematics: SurfaceGapKinematics,
    trace_rows: Vec<Vec<(usize, f64)>>,
    trace_map: Vec<usize>,
    design_map: Vec<usize>,
    sites: Vec<PatchContactSite>,
    states: usize,
    fluxes: usize,
    designs: usize,
    reference_pressure_pa: f64,
    reference_ownership: ReferenceTractionOwnership,
    gap_scale_m: f64,
    force_scale_n: f64,
}
struct PatchGeometry<S> {
    g: GapCellGeometry<S>,
    corner_gap: [S; 3],
    corner_gradient: [[[S; 3]; 6]; 3],
}

fn geometry<S: Scalar>(v: [[S; 3]; 6], minimum_area: f64) -> CaeResult<PatchGeometry<S>> {
    let mid: [[_; 3]; 3] = std::array::from_fn(|i| scale(add(v[i], v[i + 3]), S::from_f64(0.5)));
    let e1 = sub(mid[1], mid[0]);
    let e2 = sub(mid[2], mid[0]);
    let c = scale(cross(e1, e2), S::from_f64(0.5));
    let a = dot(c, c).sqrt();
    if !a.value().is_finite() || a.value() <= minimum_area {
        return Err(fail("degenerate unilateral patch"));
    }
    let n = scale(c, S::one() / a);
    let t1 = scale(e1, S::one() / dot(e1, e1).sqrt());
    let t2 = cross(n, t1);
    let d: [[_; 3]; 3] = std::array::from_fn(|i| sub(v[i + 3], v[i]));
    let gap = std::array::from_fn(|i| dot(d[i], n));
    let mut gradients = [[[S::zero(); 3]; 6]; 3];
    for node in 0..6 {
        for axis in 0..3 {
            let mut unit = [S::zero(); 3];
            unit[axis] = S::from_f64(0.5);
            let p = node % 3;
            let de1 = if p == 1 {
                unit
            } else if p == 0 {
                scale(unit, -S::one())
            } else {
                [S::zero(); 3]
            };
            let de2 = if p == 2 {
                unit
            } else if p == 0 {
                scale(unit, -S::one())
            } else {
                [S::zero(); 3]
            };
            let dc = scale(add(cross(de1, e2), cross(e1, de2)), S::from_f64(0.5));
            let dn = scale(sub(dc, scale(n, dot(n, dc))), S::one() / a);
            for i in 0..3 {
                gradients[i][node][axis] = dot(d[i], dn)
                    + if p == i {
                        n[axis] * if node < 3 { -S::one() } else { S::one() }
                    } else {
                        S::zero()
                    };
            }
        }
    }
    let dl = scale(add(add(d[0], d[1]), d[2]), S::from_f64(1. / 3.));
    let le1 = sub(v[1], v[0]);
    let le2 = sub(v[2], v[0]);
    let ue1 = sub(v[4], v[3]);
    let ue2 = sub(v[5], v[3]);
    let area = add(
        c,
        scale(cross(sub(ue1, le1), sub(ue2, le2)), S::from_f64(1. / 24.)),
    );
    let volume = dot(dl, area);
    Ok(PatchGeometry {
        g: GapCellGeometry {
            vertices_m: v,
            area_vector_m2: c,
            area_m2: a,
            normal: n,
            tangent: [t1, t2],
            local_separation_m: [dot(dl, t1), dot(dl, t2), dot(dl, n)],
            volume_m3: volume,
            hydraulic_gap_m: volume / a,
        },
        corner_gap: gap,
        corner_gradient: gradients,
    })
}

impl PatchUnilateralLaw {
    pub fn new(
        kinematics: SurfaceGapKinematics,
        native_trace: CsrMatrix,
        sites: Vec<PatchContactSite>,
        reference_pressure_pa: f64,
        gap_scale_m: f64,
        force_scale_n: f64,
    ) -> CaeResult<Self> {
        Self::new_with_reference_ownership(
            kinematics,
            native_trace,
            sites,
            reference_pressure_pa,
            gap_scale_m,
            force_scale_n,
            ReferenceTractionOwnership::PairedPatch,
        )
    }
    pub fn new_with_reference_ownership(
        kinematics: SurfaceGapKinematics,
        native_trace: CsrMatrix,
        sites: Vec<PatchContactSite>,
        reference_pressure_pa: f64,
        gap_scale_m: f64,
        force_scale_n: f64,
        reference_ownership: ReferenceTractionOwnership,
    ) -> CaeResult<Self> {
        let states = kinematics.state_size();
        let fluxes = kinematics.force_size();
        let designs = kinematics.design_size();
        if native_trace.nrows() != fluxes
            || native_trace.ncols() != states
            || sites.is_empty()
            || !reference_pressure_pa.is_finite()
            || reference_pressure_pa < 0.
            || ![gap_scale_m, force_scale_n]
                .iter()
                .all(|v| v.is_finite() && *v > 0.)
            || states.checked_add(sites.len()).is_none()
        {
            return Err(fail("invalid unilateral patch layout"));
        }
        let mut selected = BTreeSet::new();
        for site in &sites {
            if site.cell >= kinematics.cell_count()
                || site.corner >= 3
                || !selected.insert((site.cell, site.corner))
            {
                return Err(fail("duplicate or invalid unilateral patch site"));
            }
        }
        let (kinematics, trace_map, design_map) = kinematics.as_trace_kinematics()?.compact()?;
        let mut trace_rows = vec![];
        for &row in &trace_map {
            let (c, v) = native_trace.row(row);
            if v.iter().any(|v| !v.is_finite()) {
                return Err(fail("nonfinite native patch trace"));
            }
            trace_rows.push(
                c.iter()
                    .copied()
                    .zip(v.iter().copied())
                    .filter(|(_, v)| *v != 0.)
                    .collect(),
            );
        }
        Ok(Self {
            kinematics,
            trace_rows,
            trace_map,
            design_map,
            sites,
            states,
            fluxes,
            designs,
            reference_pressure_pa,
            reference_ownership,
            gap_scale_m,
            force_scale_n,
        })
    }
    pub(crate) fn release_trace_chart_gaps(&self,trace:&[f64],design:&[f64])->CaeResult<Vec<[f64;3]>> {
        if trace.len()!=self.fluxes || design.len()!=self.designs || trace.iter().chain(design).any(|v|!v.is_finite()) { return Err(fail("primal closed chart trace/design shape")); }
        let t:Vec<_>=self.trace_map.iter().map(|&i|trace[i]).collect();let d:Vec<_>=self.design_map.iter().map(|&i|design[i]).collect();
        self.kinematics.mapped_geometry_from_trace(&t,&d,|v,c|Ok(geometry(v,c.minimum_area_m2)?.corner_gap))
    }
    pub(crate) fn release_chart_gaps(&self,current:&[f64],design:&[f64])->CaeResult<Vec<[f64;3]>> {
        let(t,d,_)=self.inputs(current,design)?;
        self.kinematics.mapped_geometry_from_trace(&t,&d,|v,c|Ok(geometry(v,c.minimum_area_m2)?.corner_gap))
    }
    pub(crate) fn release_binding(
        &self,
    ) -> (
        &SurfaceGapKinematics,
        &[PatchContactSite],
        &[usize],
        &[usize],
        &[Vec<(usize, f64)>],
    ) {
        (
            &self.kinematics,
            &self.sites,
            &self.trace_map,
            &self.design_map,
            &self.trace_rows,
        )
    }
    pub fn state_size(&self) -> usize {
        self.states + self.sites.len()
    }
    pub fn reference_pressure_pa(&self) -> f64 {
        self.reference_pressure_pa
    }
    fn inputs(&self, current: &[f64], design: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>)> {
        if current.len() != self.state_size()
            || design.len() != self.designs
            || current.iter().chain(design).any(|v| !v.is_finite())
        {
            return Err(fail("unilateral patch input shape or value"));
        }
        let trace = self
            .trace_rows
            .iter()
            .map(|row| row.iter().map(|(i, w)| current[*i] * w).sum())
            .collect();
        let design = self.design_map.iter().map(|i| design[*i]).collect();
        Ok((trace, design, current[self.states..].to_vec()))
    }
    fn local<S: Scalar>(
        &self,
        trace: &[S],
        design: &[S],
        lambda: &[S],
    ) -> CaeResult<(Vec<S>, Vec<S>)> {
        let gs = self
            .kinematics
            .mapped_geometry_from_trace(trace, design, |v, c| geometry(v, c.minimum_area_m2))?;
        let mut forces = Vec::with_capacity(gs.len());
        for g in &gs {
            let pressure = self
                .reference_ownership
                .patch_pressure_pa(self.reference_pressure_pa, g.corner_gap)?;
            forces.push(pressure_loads(&g.g, S::from_f64(pressure))?.cover_n);
        }
        let mut gaps = vec![];
        for (site, &lambda) in self.sites.iter().zip(lambda) {
            let g = &gs[site.cell];
            gaps.push(g.corner_gap[site.corner]);
            for node in 0..6 {
                for axis in 0..3 {
                    forces[site.cell][node][axis] +=
                        lambda * g.corner_gradient[site.corner][node][axis];
                }
            }
        }
        Ok((self.kinematics.scatter_cover(&forces, design)?, gaps))
    }
    pub fn gaps(&self, current: &[f64], design: &[f64]) -> CaeResult<Vec<f64>> {
        let (t, d, l) = self.inputs(current, design)?;
        Ok(self.local(&t, &d, &l)?.1)
    }
    pub fn validate_unique_reference(&self, current: &[f64], design: &[f64]) -> CaeResult<()> {
        let (t, d, _) = self.inputs(current, design)?;
        let mut rows = vec![vec![0.; t.len()]; self.sites.len()];
        for first in (0..t.len()).step_by(16) {
            let dual: Vec<_> = t
                .iter()
                .enumerate()
                .map(|(j, v)| {
                    let mut seed = [0.; 16];
                    if j >= first && j < first + 16 {
                        seed[j - first] = 1.;
                    }
                    Dual::new(*v, seed)
                })
                .collect();
            let d: Vec<_> = d.iter().map(|v| Dual::<16>::constant(*v)).collect();
            let l = vec![Dual::<16>::constant(0.); self.sites.len()];
            let (_, g) = self.local(&dual, &d, &l)?;
            for (i, g) in g.iter().enumerate() {
                for k in 0..16 {
                    if first + k < t.len() {
                        rows[i][first + k] = g.eps[k];
                    }
                }
            }
        }
        for i in 0..rows.len() {
            for j in 0..i {
                if rows[i] == rows[j] {
                    return Err(fail("redundant unilateral contact constraints"));
                }
            }
        }
        Ok(())
    }
    fn path_admitted(&self, previous: &[f64], current: &[f64], design: &[f64]) -> CaeResult<()> {
        self.check_state_domain(
            0,
            previous,
            previous,
            StepParameters {
                design,
                time_scale: 1.,
            },
        )?;
        self.check_state_domain(
            0,
            current,
            previous,
            StepParameters {
                design,
                time_scale: 1.,
            },
        )?;
        let (old, d, _) = self.inputs(previous, design)?;
        let (new, _, _) = self.inputs(current, design)?;
        let old = self
            .kinematics
            .mapped_geometry_from_trace(&old, &d, |v, c| Ok((v, c.minimum_area_m2)))?;
        let new = self
            .kinematics
            .mapped_geometry_from_trace(&new, &d, |v, _| Ok(v))?;
        for (cell, ((old, minimum), new)) in old.iter().zip(&new).enumerate() {
            let m0: [[f64; 3]; 3] = std::array::from_fn(|i| scale(add(old[i], old[i + 3]), 0.5));
            let m1: [[f64; 3]; 3] = std::array::from_fn(|i| scale(add(new[i], new[i + 3]), 0.5));
            let a = sub(m0[1], m0[0]);
            let b = sub(m0[2], m0[0]);
            let da = sub(sub(m1[1], m1[0]), a);
            let db = sub(sub(m1[2], m1[0]), b);
            let c = [
                scale(cross(a, b), 0.5),
                scale(add(cross(da, b), cross(a, db)), 0.5),
                scale(cross(da, db), 0.5),
            ];
            let normal = scale(c[0], 1. / dot(c[0], c[0]).sqrt());
            if polynomial_minimum([dot(normal, c[0]), dot(normal, c[1]), dot(normal, c[2]), 0.])?
                <= *minimum
            {
                return Err(fail("unilateral patch path orientation is not certified"));
            }
            for site in self.sites.iter().filter(|s| s.cell == cell) {
                let d0 = sub(old[site.corner + 3], old[site.corner]);
                let dd = sub(sub(new[site.corner + 3], new[site.corner]), d0);
                let coefficients = [
                    dot(d0, c[0]),
                    dot(dd, c[0]) + dot(d0, c[1]),
                    dot(dd, c[1]) + dot(d0, c[2]),
                    dot(dd, c[2]),
                ];
                if polynomial_minimum(coefficients)? < 0. {
                    return Err(fail("unilateral patch path intersects"));
                }
            }
        }
        Ok(())
    }
    fn closed_corner_rows(
        &self,
        current: &[f64],
        design: &[f64],
    ) -> CaeResult<crate::moving_contact::contact_field::ContactClosedConstraintRows> {
        let (t, d, _) = self.inputs(current, design)?;
        let base = self
            .kinematics
            .mapped_geometry_from_trace(&t, &d, |v, c| geometry(v, c.minimum_area_m2))?;
        let closed: Vec<_> = base
            .iter()
            .enumerate()
            .filter(|(_, g)| g.corner_gap.iter().all(|v| *v == 0.))
            .map(|(i, _)| i)
            .collect();
        let mut cur = vec![];
        let mut des = vec![];
        let count = t.len() + d.len();
        for first in (0..count).step_by(16) {
            let lift = |v: f64, j: usize| {
                let mut eps = [0.; 16];
                if j >= first && j < first + 16 {
                    eps[j - first] = 1.;
                }
                Dual::new(v, eps)
            };
            let tt: Vec<_> = t.iter().enumerate().map(|(j, &v)| lift(v, j)).collect();
            let dd: Vec<_> = d
                .iter()
                .enumerate()
                .map(|(j, &v)| lift(v, t.len() + j))
                .collect();
            let values = self
                .kinematics
                .mapped_geometry_from_trace(&tt, &dd, |v, c| geometry(v, c.minimum_area_m2))?;
            for (row, &cell) in closed.iter().enumerate() {
                for corner in 0..3 {
                    for k in 0..16 {
                        let j = first + k;
                        if j >= count {
                            break;
                        }
                        let v = values[cell].corner_gap[corner].eps[k];
                        if v == 0. {
                            continue;
                        }
                        if j < t.len() {
                            for &(column, weight) in &self.trace_rows[j] {
                                cur.push((3 * row + corner, column, v * weight));
                            }
                        } else {
                            des.push((3 * row + corner, self.design_map[j - t.len()], v));
                        }
                    }
                }
            }
        }
        Ok(
            crate::moving_contact::contact_field::ContactClosedConstraintRows {
                current: matrix(3 * closed.len(), self.state_size(), &cur)?,
                previous: matrix(3 * closed.len(), self.state_size(), &[])?,
                design: matrix(3 * closed.len(), self.designs, &des)?,
                time: vec![0.; 3 * closed.len()],
            },
        )
    }
    fn contribution(&self, current: &[f64], design: &[f64]) -> CaeResult<ContactContribution> {
        self.contribution_mode(current, design, false)
    }
    fn contribution_mode(
        &self,
        current: &[f64],
        design: &[f64],
        raw_gap: bool,
    ) -> CaeResult<ContactContribution> {
        let (t, d, l) = self.inputs(current, design)?;
        let (f, g) = self.local(&t, &d, &l)?;
        let parts = g
            .iter()
            .zip(&l)
            .map(|(g, l)| {
                residual_partials(
                    *g,
                    *l,
                    self.gap_scale_m,
                    self.force_scale_n,
                    OriginBranch::Closed,
                )
            })
            .collect::<CaeResult<Vec<_>>>()?;
        let mut fc = vec![];
        let mut gc = vec![];
        let mut fd = vec![];
        let mut gd = vec![];
        let count = t.len() + l.len() + d.len();
        for first in (0..count).step_by(16) {
            let lift = |v: f64, j: usize| {
                let mut s = [0.; 16];
                if j >= first && j < first + 16 {
                    s[j - first] = 1.;
                }
                Dual::new(v, s)
            };
            let tt: Vec<_> = t.iter().enumerate().map(|(i, v)| lift(*v, i)).collect();
            let ll: Vec<_> = l
                .iter()
                .enumerate()
                .map(|(i, v)| lift(*v, t.len() + i))
                .collect();
            let dd: Vec<_> = d
                .iter()
                .enumerate()
                .map(|(i, v)| lift(*v, t.len() + l.len() + i))
                .collect();
            let (ff, gg) = self.local(&tt, &dd, &ll)?;
            for k in 0..16 {
                let j = first + k;
                if j >= count {
                    break;
                }
                let targets: Vec<(usize, f64)> = if j < t.len() {
                    self.trace_rows[j].clone()
                } else if j < t.len() + l.len() {
                    vec![(self.states + j - t.len(), 1.)]
                } else {
                    vec![(self.design_map[j - t.len() - l.len()], 1.)]
                };
                let is_design = j >= t.len() + l.len();
                for (i, v) in ff.iter().enumerate() {
                    for &(column, w) in &targets {
                        let value = v.eps[k] * w;
                        if value != 0. {
                            if is_design {
                                fd.push((self.trace_map[i], column, value));
                            } else {
                                fc.push((self.trace_map[i], column, value));
                            }
                        }
                    }
                }
                for (i, v) in gg.iter().enumerate() {
                    let value = if raw_gap {
                        v.eps[k]
                    } else {
                        parts[i].1 * v.eps[k] + if j == t.len() + i { parts[i].2 } else { 0. }
                    };
                    for &(column, w) in &targets {
                        if value * w != 0. {
                            if is_design {
                                gd.push((i, column, value * w));
                            } else {
                                gc.push((i, column, value * w));
                            }
                        }
                    }
                }
            }
        }
        let mut force = vec![0.; self.fluxes];
        for (&i, &v) in self.trace_map.iter().zip(&f) {
            force[i] = v;
        }
        Ok(ContactContribution {
            force,
            constraints: if raw_gap {
                g.clone()
            } else {
                parts.iter().map(|p| p.0).collect()
            },
            force_current: matrix(self.fluxes, self.state_size(), &fc)?,
            force_previous: matrix(self.fluxes, self.state_size(), &[])?,
            force_design: matrix(self.fluxes, self.designs, &fd)?,
            force_time: vec![0.; self.fluxes],
            constraint_current: matrix(self.sites.len(), self.state_size(), &gc)?,
            constraint_previous: matrix(self.sites.len(), self.state_size(), &[])?,
            constraint_design: matrix(self.sites.len(), self.designs, &gd)?,
            constraint_time: vec![0.; self.sites.len()],
        })
    }
}

impl NativeContactLaw for PatchUnilateralLaw {
    fn newton_direction_constraints(
        &self,
        _n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        direction: &[f64],
    ) -> CaeResult<Vec<(usize, f64)>> {
        if direction.len() != self.state_size() || direction.iter().any(|v| !v.is_finite()) {
            return Err(fail("patch contact Newton direction shape"));
        }
        self.inputs(previous, p.design)?;
        let gaps = self.gaps(current, p.design)?;
        let contribution = self.contribution(current, p.design)?;
        let rows = contribution.constraint_current.to_csr()?;
        let mut constraints = Vec::new();
        for (i, gap) in gaps.into_iter().enumerate() {
            let column = self.states + i;
            if gap > 0. && current[column] == 0. {
                let (columns, values) = rows.row(i);
                if columns != [column]
                    || values.len() != 1
                    || values[0] != -1. / self.force_scale_n
                    || contribution.constraints[i] != 0.
                {
                    return Err(fail("open contact equation is not an exact inactive row"));
                }
                constraints.push((column, 0.));
            }
        }
        Ok(constraints)
    }
    fn multipliers(&self) -> usize {
        self.sites.len()
    }
    fn evaluate(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<ContactContribution> {
        self.contribution(current, p.design)
    }
    fn trace_constraint_data(
        &self,
        _n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Option<crate::moving_contact::contact_field::ContactTraceConstraintPair>> {
        let endpoint = |state: &[f64]| -> CaeResult<_> {
            let gaps = self.gaps(state, p.design)?;
            let branches = gaps
                .iter()
                .zip(&state[self.states..])
                .map(
                    |(&gap_m, &multiplier_n)| crate::contact_constraint_tangent::ContactBranch {
                        gap_m,
                        multiplier_n,
                    },
                )
                .collect();
            Ok(
                crate::moving_contact::contact_field::ContactTraceConstraintData {
                    contribution: self.contribution_mode(state, p.design, true)?,
                    closed_rows: Some(self.closed_corner_rows(state, p.design)?),
                    branches,
                    origins: std::collections::BTreeMap::new(),
                },
            )
        };
        Ok(Some(
            crate::moving_contact::contact_field::ContactTraceConstraintPair {
                start: endpoint(previous)?,
                end: endpoint(current)?,
            },
        ))
    }
    fn trace_constraint_data_for_direction(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        direction: &[f64],
    ) -> CaeResult<Option<crate::moving_contact::contact_field::ContactTraceConstraintPair>> {
        if direction.len() != self.state_size() || direction.iter().any(|v| !v.is_finite()) {
            return Err(fail("patch contact trace predictor shape"));
        }
        let mut data = self
            .trace_constraint_data(n, current, previous, p)?
            .ok_or_else(|| fail("missing patch contact trace data"))?;
        let matrix = data.end.contribution.constraint_current.to_csr()?;
        for (i, branch) in data.end.branches.iter().enumerate() {
            if branch.gap_m != 0. || branch.multiplier_n != 0. {
                continue;
            }
            let (columns, values) = matrix.row(i);
            let terms: Vec<_> = columns
                .iter()
                .zip(values)
                .map(|(&j, &v)| v * direction[j])
                .collect();
            let dg: f64 = terms.iter().sum();
            let dl = direction[self.states + i];
            let gap_roundoff = 128.
                * f64::EPSILON
                * terms
                    .iter()
                    .map(|v| v.abs())
                    .sum::<f64>()
                    .max(f64::MIN_POSITIVE);
            let force_roundoff = 128. * f64::EPSILON * dl.abs().max(f64::MIN_POSITIVE);
            let selected = if dg > gap_roundoff && dl.abs() <= force_roundoff {
                crate::contact_constraint_tangent::ContactOriginSelection::Open
            } else if dg.abs() <= gap_roundoff && dl >= -force_roundoff {
                crate::contact_constraint_tangent::ContactOriginSelection::Closed
            } else {
                return Err(fail(
                    "patch contact predictor leaves the complementarity cone",
                ));
            };
            data.end.origins.insert(i, selected);
        }
        Ok(Some(data))
    }
    fn check_trace_derivative_domain(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()> {
        let g = self.gaps(current, p.design)?;
        if g.iter()
            .zip(&current[self.states..])
            .any(|(g, l)| *g == 0. && *l == 0.)
        {
            return Err(fail(
                "biactive patch contact has no ordinary constrained derivative",
            ));
        }
        Ok(())
    }
    fn check_derivative_domain(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()> {
        let g = self.gaps(current, p.design)?;
        if g.iter()
            .zip(&current[self.states..])
            .any(|(g, l)| *g == 0. && *l == 0.)
        {
            return Err(fail("biactive patch contact has no ordinary derivative"));
        }
        Ok(())
    }
    fn check_state_domain(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()> {
        let g = self.gaps(current, p.design)?;
        if g.iter().any(|g| *g < 0.) || current[self.states..].iter().any(|l| *l < 0.) {
            return Err(fail("unilateral patch gap or reaction is negative"));
        }
        Ok(())
    }
    fn newton_constraints(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        p: StepParameters<'_>,
        direction: &[f64],
        attempt: usize,
    ) -> CaeResult<Option<Jacobian>> {
        if direction.len() != self.state_size() || direction.iter().any(|v| !v.is_finite()) {
            return Err(fail("unilateral patch origin direction"));
        }
        let contribution = self.contribution(current, p.design)?;
        let gaps = self.gaps(current, p.design)?;
        let mut rows = vec![];
        let original = contribution.constraint_current.to_csr()?;
        let mut changed = false;
        for i in 0..self.sites.len() {
            let (c, v) = original.row(i);
            if gaps[i] == 0. && current[self.states + i] == 0. && direction[self.states + i] < 0. {
                if attempt > 0 {
                    return Err(fail("patch contact origin alternatives exhausted"));
                }
                rows.push((i, self.states + i, -1. / self.force_scale_n));
                changed = true;
            } else {
                for (&c, &v) in c.iter().zip(v) {
                    rows.push((i, c, v));
                }
            }
        }
        if changed {
            Ok(Some(matrix(self.sites.len(), self.state_size(), &rows)?))
        } else {
            Ok(None)
        }
    }
    fn admitted_trial(
        &self,
        _n: usize,
        current: &[f64],
        direction: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        trial: f64,
    ) -> CaeResult<f64> {
        if direction.len() != self.state_size()
            || direction.iter().any(|v| !v.is_finite())
            || !trial.is_finite()
            || trial <= 0.
            || trial > 1.
        {
            return Err(fail("unilateral patch trial"));
        }
        self.inputs(previous, p.design)?;
        let mut fraction = trial;
        for _ in 0..40 {
            let q: Vec<_> = current
                .iter()
                .zip(direction)
                .map(|(q, d)| q + fraction * d)
                .collect();
            if self.path_admitted(previous, &q, p.design).is_ok() {
                return Ok(fraction);
            }
            fraction *= 0.5;
        }
        Ok(0.)
    }
}

fn polynomial_minimum(coefficients: [f64; 4]) -> CaeResult<f64> {
    if coefficients.iter().any(|x| !x.is_finite()) {
        return Err(fail("patch path polynomial overflow"));
    }
    let scale = coefficients.iter().map(|x| x.abs()).fold(0., f64::max);
    if scale == 0. {
        return Ok(0.);
    }
    let a = coefficients.map(|v| v / scale);
    let value = |t: f64| a[0] + t * (a[1] + t * (a[2] + t * a[3]));
    let mut minimum = value(0.).min(value(1.));
    let aa = 3. * a[3];
    let bb = 2. * a[2];
    let cc = a[1];
    let mut roots = vec![];
    if aa == 0. {
        if bb != 0. {
            roots.push(-cc / bb);
        }
    } else {
        let disc = bb * bb - 4. * aa * cc;
        if disc >= 0. {
            let q = -0.5 * (bb + disc.sqrt().copysign(bb));
            if q != 0. {
                roots.push(q / aa);
                roots.push(cc / q);
            } else {
                roots.push(-bb / (2. * aa));
            }
        }
    }
    for t in roots {
        if t > 0. && t < 1. {
            minimum = minimum.min(value(t));
        }
    }
    let result = minimum * scale;
    if !result.is_finite() {
        return Err(fail("patch path minimum overflow"));
    }
    Ok(result)
}
