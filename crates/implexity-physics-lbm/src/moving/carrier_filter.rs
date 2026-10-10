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

use super::carrier::{LagrangianCarrier, PointCloud, SecondOrderCarrier};
use super::pushforward::{Frame, Stencils};
use implexity_core::{CaeError, CaeResult};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct CarrierVolumePartition {
    pub envelope_volume_m3: f64,
    pub solid_volume_m3: f64,
    pub gap_volume_m3: f64,
    pub unrepresented_volume_m3: f64,
}

pub struct FilteredCarrier {
    inner: Arc<dyn LagrangianCarrier>,
    kept: Vec<usize>,
    removed: Vec<usize>,
    ownership: Option<(Frame, Vec<usize>, Vec<bool>)>,
}

impl FilteredCarrier {
    pub fn new(inner: Arc<dyn LagrangianCarrier>, mut removed: Vec<usize>) -> CaeResult<Self> {
        removed.sort_unstable();
        if removed.iter().any(|i| *i >= inner.point_count())
            || removed.windows(2).any(|w| w[0] == w[1])
        {
            return Err(CaeError::contract(
                "carrier filter requires unique in-range owned points",
            ));
        }
        let kept = (0..inner.point_count())
            .filter(|i| removed.binary_search(i).is_err())
            .collect();
        Ok(Self {
            inner,
            kept,
            removed,
            ownership: None,
        })
    }

    pub fn with_runtime_ownership(
        mut self,
        trace: &[f64],
        design: &[f64],
        frame: Frame,
        excluded: Vec<usize>,
        original_static_mask: Vec<bool>,
    ) -> CaeResult<Self> {
        self.validate_owned_or_static_cells(
            trace,
            design,
            &frame,
            &excluded,
            &original_static_mask,
        )?;
        self.ownership = Some((frame, excluded, original_static_mask));
        Ok(self)
    }

    fn validate_runtime_ownership(&self, trace: &[f64], design: &[f64]) -> CaeResult<()> {
        if let Some((frame, excluded, mask)) = &self.ownership {
            self.validate_owned_or_static_cells(trace, design, frame, excluded, mask)?;
        }
        Ok(())
    }

    pub fn retained_indices(&self) -> &[usize] {
        &self.kept
    }
    pub fn removed_indices(&self) -> &[usize] {
        &self.removed
    }

    fn arguments(&self, trace: &[f64], design: &[f64]) -> CaeResult<()> {
        if trace.len() != self.trace_size()
            || design.len() != self.design_size()
            || trace.iter().chain(design).any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "carrier filter requires finite matching trace and design",
            ));
        }
        Ok(())
    }

    fn cloud(&self, cloud: PointCloud) -> CaeResult<PointCloud> {
        if cloud.positions.len() != self.inner.point_count()
            || cloud.weights.len() != self.inner.point_count()
            || cloud
                .positions
                .iter()
                .flatten()
                .chain(cloud.weights.iter())
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "carrier filter received an invalid point cloud",
            ));
        }
        Ok(PointCloud {
            positions: self.kept.iter().map(|i| cloud.positions[*i]).collect(),
            weights: self.kept.iter().map(|i| cloud.weights[*i]).collect(),
        })
    }

    fn scatter(
        &self,
        positions: &[[f64; 3]],
        weights: &[f64],
    ) -> CaeResult<(Vec<[f64; 3]>, Vec<f64>)> {
        if positions.len() != self.kept.len()
            || weights.len() != self.kept.len()
            || positions
                .iter()
                .flatten()
                .chain(weights.iter())
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "carrier filter requires matching finite point cotangents",
            ));
        }
        let mut p = vec![[0.0; 3]; self.inner.point_count()];
        let mut w = vec![0.0; self.inner.point_count()];
        for (j, i) in self.kept.iter().enumerate() {
            p[*i] = positions[j];
            w[*i] = weights[j];
        }
        Ok((p, w))
    }

    pub fn validate_volume_partition(
        &self,
        trace: &[f64],
        design: &[f64],
        frame: &Frame,
        excluded: &[usize],
        solid_mask: &[bool],
        saturation_width: f64,
        gap_volume_m3: f64,
        absolute_tolerance_m3: f64,
        relative_tolerance: f64,
    ) -> CaeResult<CarrierVolumePartition> {
        if !absolute_tolerance_m3.is_finite()
            || absolute_tolerance_m3 < 0.0
            || !relative_tolerance.is_finite()
            || relative_tolerance < 0.0
        {
            return Err(CaeError::contract(
                "invalid native carrier partition tolerance",
            ));
        }
        let receipt = self.volume_partition(
            trace,
            design,
            frame,
            excluded,
            solid_mask,
            saturation_width,
            gap_volume_m3,
        )?;
        let tolerance = absolute_tolerance_m3 + relative_tolerance * receipt.envelope_volume_m3;
        if !tolerance.is_finite() || receipt.unrepresented_volume_m3.abs() > tolerance {
            return Err(CaeError::contract(format!(
                "native gap partition volume defect {} m3 exceeds policy",
                receipt.unrepresented_volume_m3
            )));
        }
        Ok(receipt)
    }

    pub fn volume_partition(
        &self,
        trace: &[f64],
        design: &[f64],
        frame: &Frame,
        excluded: &[usize],
        solid_mask: &[bool],
        saturation_width: f64,
        gap_volume_m3: f64,
    ) -> CaeResult<CarrierVolumePartition> {
        self.validate_excluded_cells(trace, design, frame, excluded, solid_mask)?;
        if !saturation_width.is_finite()
            || saturation_width <= 0.0
            || saturation_width >= 1.0
            || !gap_volume_m3.is_finite()
            || gap_volume_m3 <= 0.0
        {
            return Err(CaeError::contract(
                "invalid native carrier volume partition policy",
            ));
        }
        let cloud = self.inner.points(trace, design)?;
        let xi: Vec<_> = cloud
            .positions
            .iter()
            .map(|x| frame.to_lattice(*x))
            .collect();
        if xi.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "carrier filter lattice coordinates exceed finite geometry",
            ));
        }
        let stencil = Stencils::new(frame, &xi, false)?;
        let stride = 4 * frame.width_cells;
        let cell_volume = frame.spacing_m.powi(3);
        if !(cell_volume.is_finite() && cell_volume > 0.0) {
            return Err(CaeError::contract("invalid native carrier cell volume"));
        }
        let mut occupancy = vec![0.0; solid_mask.len()];
        for i in 0..self.inner.point_count() {
            for x in 0..frame.support(0) {
                for y in 0..frame.support(1) {
                    for z in 0..frame.support(2) {
                        let w = |a: usize, t: usize| stencil.w[i * 3 * stride + a * stride + t];
                        let ix = (stencil.base[i][0] + x) % frame.shape[0];
                        let iy = (stencil.base[i][1] + y) % frame.shape[1];
                        let iz = (stencil.base[i][2] + z) % frame.shape[2];
                        occupancy[(ix * frame.shape[1] + iy) * frame.shape[2] + iz] +=
                            cloud.weights[i] / cell_volume * w(0, x) * w(1, y) * w(2, z);
                    }
                }
            }
        }
        let envelope = excluded.len() as f64 * cell_volume;
        let solid = excluded
            .iter()
            .map(|i| super::psm::saturate(occupancy[*i], saturation_width) * cell_volume)
            .sum::<f64>();
        let defect = envelope - solid - gap_volume_m3;
        if !solid.is_finite() || !defect.is_finite() {
            return Err(CaeError::contract(format!(
                "native gap partition volume defect {defect} m3 exceeds policy"
            )));
        }
        Ok(CarrierVolumePartition {
            envelope_volume_m3: envelope,
            solid_volume_m3: solid,
            gap_volume_m3,
            unrepresented_volume_m3: defect,
        })
    }

    pub fn validate_owned_or_static_cells(
        &self,
        trace: &[f64],
        design: &[f64],
        frame: &Frame,
        excluded: &[usize],
        original_static_mask: &[bool],
    ) -> CaeResult<(usize, usize)> {
        self.arguments(trace, design)?;
        let cloud = self.inner.points(trace, design)?;
        self.validate_owned_cloud(&cloud, frame, excluded, original_static_mask)
    }

    fn validate_owned_cloud(
        &self,
        cloud: &PointCloud,
        frame: &Frame,
        excluded: &[usize],
        original_static_mask: &[bool],
    ) -> CaeResult<(usize, usize)> {
        let cells = frame
            .shape
            .iter()
            .try_fold(1usize, |a, b| a.checked_mul(*b))
            .ok_or_else(|| CaeError::contract("carrier ownership lattice size overflow"))?;
        if original_static_mask.len() != cells || excluded.is_empty() {
            return Err(CaeError::contract(
                "carrier ownership static mask or owned region shape",
            ));
        }
        let mut owned = vec![false; cells];
        for &c in excluded {
            if c >= cells || owned[c] || original_static_mask[c] {
                return Err(CaeError::contract(
                    "carrier ownership requires unique physical fluid cells",
                ));
            }
            owned[c] = true;
        }
        if cloud.positions.len() != self.inner.point_count()
            || cloud.weights.len() != self.inner.point_count()
            || cloud.positions.iter().flatten().any(|v| !v.is_finite())
            || cloud.weights.iter().any(|v| !v.is_finite() || *v < 0.)
        {
            return Err(CaeError::contract(
                "carrier ownership received invalid native cloud",
            ));
        }
        let xi: Vec<_> = cloud
            .positions
            .iter()
            .map(|&p| frame.to_lattice(p))
            .collect();
        let stencil = Stencils::new(frame, &xi, false)?;
        let stride = 4 * frame.width_cells;
        let (mut physical_owned, mut static_only) = (0, 0);
        for i in 0..self.inner.point_count() {
            if cloud.weights[i] == 0. {
                continue;
            }
            let removed = self.removed.binary_search(&i).is_ok();
            let mut touches = false;
            for x in 0..frame.support(0) {
                for y in 0..frame.support(1) {
                    for z in 0..frame.support(2) {
                        let w = stencil.w[i * 3 * stride + x]
                            * stencil.w[i * 3 * stride + stride + y]
                            * stencil.w[i * 3 * stride + 2 * stride + z];
                        if w == 0. {
                            continue;
                        }
                        let ix = (stencil.base[i][0] + x) % frame.shape[0];
                        let iy = (stencil.base[i][1] + y) % frame.shape[1];
                        let iz = (stencil.base[i][2] + z) % frame.shape[2];
                        let c = (ix * frame.shape[1] + iy) * frame.shape[2] + iz;
                        if removed && !owned[c] && !original_static_mask[c] {
                            return Err(CaeError::contract(
                                "removed native point couples to unowned physical bulk",
                            ));
                        }
                        if !removed && owned[c] {
                            return Err(CaeError::contract(
                                "retained native point couples to owned gas cells",
                            ));
                        }
                        touches |= owned[c];
                    }
                }
            }
            if removed {
                if touches {
                    physical_owned += 1;
                } else {
                    static_only += 1;
                }
            }
        }
        Ok((physical_owned, static_only))
    }
    pub fn validate_excluded_cells(
        &self,
        trace: &[f64],
        design: &[f64],
        frame: &Frame,
        excluded: &[usize],
        solid_mask: &[bool],
    ) -> CaeResult<()> {
        self.arguments(trace, design)?;
        let cells = frame
            .shape
            .iter()
            .try_fold(1usize, |a, b| a.checked_mul(*b))
            .ok_or_else(|| CaeError::contract("carrier filter lattice size overflow"))?;
        if solid_mask.len() != cells {
            return Err(CaeError::contract(
                "carrier filter requires matching static solid mask",
            ));
        }
        let mut owned = vec![false; cells];
        if excluded.is_empty() {
            return Err(CaeError::contract(
                "carrier filter requires explicit excluded cells",
            ));
        }
        for i in excluded {
            if *i >= cells || owned[*i] || !solid_mask[*i] {
                return Err(CaeError::contract(
                    "carrier filter requires unique in-range excluded cells",
                ));
            }
            owned[*i] = true;
        }
        let cloud = self.inner.points(trace, design)?;
        if cloud.positions.len() != self.inner.point_count()
            || cloud.weights.len() != self.inner.point_count()
            || cloud.positions.iter().flatten().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "carrier filter received an invalid ownership cloud",
            ));
        }
        let xi: Vec<_> = cloud
            .positions
            .iter()
            .map(|x| frame.to_lattice(*x))
            .collect();
        if xi.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "carrier filter lattice coordinates exceed finite geometry",
            ));
        }
        let stencil = Stencils::new(frame, &xi, false)?;
        let stride = 4 * frame.width_cells;
        let weight_at = |point: usize, axis: usize, offset: usize| {
            stencil.w[point * 3 * stride + axis * stride + offset]
        };
        for i in 0..self.inner.point_count() {
            if !(cloud.weights[i].is_finite() && cloud.weights[i] >= 0.0) {
                return Err(CaeError::contract(
                    "carrier filter requires nonnegative finite point weights",
                ));
            }
            if cloud.weights[i] == 0.0 {
                continue;
            }
            let removed = self.removed.binary_search(&i).is_ok();
            let mut touches = false;
            for x in 0..frame.support(0) {
                for y in 0..frame.support(1) {
                    for z in 0..frame.support(2) {
                        let weight = weight_at(i, 0, x) * weight_at(i, 1, y) * weight_at(i, 2, z);
                        if weight == 0.0 {
                            continue;
                        }
                        let ix = (stencil.base[i][0] + x) % frame.shape[0];
                        let iy = (stencil.base[i][1] + y) % frame.shape[1];
                        let iz = (stencil.base[i][2] + z) % frame.shape[2];
                        let c = (ix * frame.shape[1] + iy) * frame.shape[2] + iz;
                        if removed && !solid_mask[c] {
                            return Err(CaeError::contract(
                                "owned carrier point has support in active bulk cells",
                            ));
                        }
                        if owned[c] {
                            touches = true;
                        }
                    }
                }
            }
            if removed && !touches {
                return Err(CaeError::contract(
                    "owned carrier point does not touch excluded gap cells",
                ));
            }
        }
        Ok(())
    }
}

impl LagrangianCarrier for FilteredCarrier {
    fn trace_size(&self) -> usize {
        self.inner.trace_size()
    }
    fn design_size(&self) -> usize {
        self.inner.design_size()
    }
    fn point_count(&self) -> usize {
        self.kept.len()
    }
    fn points(&self, trace: &[f64], design: &[f64]) -> CaeResult<PointCloud> {
        self.arguments(trace, design)?;
        let cloud = self.inner.points(trace, design)?;
        if let Some((frame, excluded, mask)) = &self.ownership {
            self.validate_owned_cloud(&cloud, frame, excluded, mask)?;
        }
        self.cloud(cloud)
    }
    fn points_jvp(
        &self,
        trace: &[f64],
        design: &[f64],
        d_trace: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<PointCloud> {
        self.arguments(trace, design)?;
        self.validate_runtime_ownership(trace, design)?;
        if d_trace.len() != self.trace_size()
            || d_trace.iter().any(|v| !v.is_finite())
            || d_design
                .is_some_and(|d| d.len() != self.design_size() || d.iter().any(|v| !v.is_finite()))
        {
            return Err(CaeError::contract(
                "carrier filter requires matching finite point directions",
            ));
        }
        self.cloud(self.inner.points_jvp(trace, design, d_trace, d_design)?)
    }
    fn points_vjp(
        &self,
        trace: &[f64],
        design: &[f64],
        positions_bar: &[[f64; 3]],
        weights_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.arguments(trace, design)?;
        self.validate_runtime_ownership(trace, design)?;
        let (p, w) = self.scatter(positions_bar, weights_bar)?;
        self.inner.points_vjp(trace, design, &p, &w)
    }
    fn impulses_to_flux(&self, impulses: &[[f64; 3]], dt: f64) -> Vec<f64> {
        let mut full = vec![[0.0; 3]; self.inner.point_count()];
        for (j, i) in self.kept.iter().enumerate() {
            full[*i] = impulses[j];
        }
        self.inner.impulses_to_flux(&full, dt)
    }
    fn flux_to_impulses(&self, flux: &[f64], dt: f64) -> Vec<[f64; 3]> {
        let full = self.inner.flux_to_impulses(flux, dt);
        self.kept.iter().map(|i| full[*i]).collect()
    }
    fn velocities(&self, start: &[f64], end: &[f64], dt: f64) -> Vec<[f64; 3]> {
        let full = self.inner.velocities(start, end, dt);
        self.kept.iter().map(|i| full[*i]).collect()
    }
    fn second_order(&self) -> Option<&dyn SecondOrderCarrier> {
        self.inner
            .second_order()
            .map(|_| self as &dyn SecondOrderCarrier)
    }
}

impl SecondOrderCarrier for FilteredCarrier {
    fn points_vjp_tangent(
        &self,
        trace: &[f64],
        design: &[f64],
        positions_bar: &[[f64; 3]],
        weights_bar: &[f64],
        d_trace: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.arguments(trace, design)?;
        self.validate_runtime_ownership(trace, design)?;
        if d_trace.len() != self.trace_size()
            || d_trace.iter().any(|v| !v.is_finite())
            || d_design
                .is_some_and(|d| d.len() != self.design_size() || d.iter().any(|v| !v.is_finite()))
        {
            return Err(CaeError::contract(
                "carrier filter requires matching finite second-order directions",
            ));
        }
        let (p, w) = self.scatter(positions_bar, weights_bar)?;
        self.inner
            .second_order()
            .ok_or_else(|| CaeError::contract("carrier has no second-order point map"))?
            .points_vjp_tangent(trace, design, &p, &w, d_trace, d_design)
    }
}
