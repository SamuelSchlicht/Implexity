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

use crate::{
    native_cut_cell_partition::{NativeCutCellLayout, NativeCutCellPartition},
    native_cut_cell_refinement::RefinedCutCellPartition,
};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use std::collections::{BTreeMap, BTreeSet};
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone, Debug)]
pub struct AggregatedGasFace {
    pub aperture: usize,
    pub left_slot: usize,
    pub right_slot: Option<usize>,
}
#[derive(Clone, Debug)]
pub struct AggregatedGasGeometry<S: Scalar> {
    pub volumes_m3: Vec<S>,
    pub centroids_m: Vec<[S; 3]>,
    pub central_covariance_m2: Vec<Option<[[S; 3]; 3]>>,
    pub active_slots: Vec<bool>,
    pub component_slots: Vec<usize>,
    pub faces: Vec<AggregatedGasFace>,
}
#[derive(Clone, Debug)]
pub struct NativeGasStateLayout {
    regions: Vec<usize>,
    cells: Vec<usize>,
    residual_slots_per_cell: usize,
    populations: usize,
    residual: NativeCutCellLayout,
    state_size: usize,
}
#[derive(Clone, Debug)]
pub struct AggregatedGasInventoryRemap<S: Scalar> {
    pub inventories_kg: Vec<S>,
    pub geometry: AggregatedGasGeometry<S>,
    pub maximum_population_balance_defect_kg: f64,
    pub maximum_unrepresented_residual_volume_m3: f64,
}
pub struct SparseGasCoefficientTransfer<S: Scalar> {
    pub previous_slot: usize,
    pub current_slot: usize,
    pub weight: S,
}
pub struct SparseGasCoefficientTranspose {
    pub coefficient_inventory_bars: Vec<Option<Vec<f64>>>,
    pub fixed_zero_previous_slots: Vec<usize>,
}
pub struct SparseGasCoefficientRemap<S: Scalar> {
    pub transfers: Vec<SparseGasCoefficientTransfer<S>>,
    pub coefficient_inventory: Vec<Option<Vec<S>>>,
    pub geometry: AggregatedGasGeometry<S>,
    pub maximum_coefficient_balance_defect: f64,
    pub maximum_unrepresented_residual_volume_m3: f64,
}
impl NativeGasStateLayout {
    pub fn new(
        mut regions: Vec<usize>,
        mut cells: Vec<usize>,
        residual_slots_per_cell: usize,
        populations: usize,
    ) -> CaeResult<Self> {
        regions.sort_unstable();
        cells.sort_unstable();
        if regions.windows(2).any(|w| w[0] == w[1]) {
            return Err(fail("duplicate numerical gas region"));
        }
        let residual =
            NativeCutCellLayout::new(cells.clone(), residual_slots_per_cell, populations)?;
        let state_size = regions
            .len()
            .checked_mul(populations)
            .and_then(|n| n.checked_add(residual.state_size()))
            .ok_or_else(|| fail("aggregated gas state size overflow"))?;
        Ok(Self {
            regions,
            cells,
            residual_slots_per_cell,
            populations,
            residual,
            state_size,
        })
    }
    pub(crate) fn binding_identity(&self) -> String {
        implexity_core::json::canonical_sha256(
            &serde_json::json!({"regions":self.regions,"cells":self.cells,"residual_slots_per_cell":self.residual_slots_per_cell,"legacy_row_width":self.populations,"state_size":self.state_size}),
        )
    }
    pub fn state_size(&self) -> usize {
        self.state_size
    }
    pub fn slots(&self) -> usize {
        self.state_size / self.populations
    }
    pub fn aggregate<S: Scalar>(
        &self,
        input: &RefinedCutCellPartition<S>,
    ) -> CaeResult<AggregatedGasGeometry<S>> {
        let p = &input.partition;
        if p.components
            .windows(2)
            .any(|w| (w[0].cell, w[0].component) >= (w[1].cell, w[1].component))
        {
            return Err(fail("gas components are not in canonical order"));
        }
        if p.components.len() != input.regions.len() {
            return Err(fail("gas region metadata shape"));
        }
        let mut volumes = vec![S::zero(); self.slots()];
        let mut active = vec![false; self.slots()];
        let mut moments = vec![[S::zero(); 3]; self.slots()];
        let mut component_slots = vec![];
        let mut residual_count: BTreeMap<usize, usize> = BTreeMap::new();
        let mut region_components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (i, (c, region)) in p.components.iter().zip(&input.regions).enumerate() {
            if !c.volume_m3.value().is_finite() || c.volume_m3.value() <= 0. {
                return Err(fail("invalid aggregated gas component volume"));
            }
            let slot = if let Some(region) = region {
                let slot = self
                    .regions
                    .binary_search(region)
                    .map_err(|_| fail("unknown numerical gas region"))?;
                region_components.entry(slot).or_default().push(i);
                slot
            } else {
                let cell = self
                    .cells
                    .binary_search(&c.cell)
                    .map_err(|_| fail("residual gas cell absent from state layout"))?;
                let local = residual_count.entry(c.cell).or_default();
                if *local >= self.residual_slots_per_cell {
                    return Err(fail(&format!(
                        "residual gas component slots exhausted in cell {} component {} volume {:e} capacity {}",
                        c.cell,
                        *local,
                        c.volume_m3.value(),
                        self.residual_slots_per_cell
                    )));
                }
                let slot = self.regions.len() + cell * self.residual_slots_per_cell + *local;
                *local += 1;
                slot
            };
            volumes[slot] += c.volume_m3;
            for k in 0..3 {
                moments[slot][k] += c.volume_m3 * c.centroid_m[k];
            }
            active[slot] = true;
            component_slots.push(slot);
        }
        let mut adjacency = vec![Vec::new(); p.components.len()];
        for a in &p.apertures {
            if a.left >= p.components.len()
                || a.right.is_some_and(|j| j >= p.components.len())
                || !a.area_m2.value().is_finite()
                || a.area_m2.value() <= 0.
            {
                return Err(fail("invalid aggregated gas aperture"));
            }
            if let Some(j) = a.right {
                if component_slots[a.left] == component_slots[j] {
                    adjacency[a.left].push(j);
                    adjacency[j].push(a.left);
                }
            }
        }
        for ids in region_components.values() {
            let expected: BTreeSet<_> = ids.iter().copied().collect();
            let mut reached = BTreeSet::new();
            let mut pending = vec![ids[0]];
            while let Some(i) = pending.pop() {
                if reached.insert(i) {
                    pending.extend(adjacency[i].iter().copied());
                }
            }
            if reached != expected {
                return Err(fail("disconnected gas pieces share a numerical region"));
            }
        }
        let mut faces = vec![];
        for (aperture, a) in p.apertures.iter().enumerate() {
            let left = component_slots[a.left];
            let right = a.right.map(|j| component_slots[j]);
            if right == Some(left) {
                continue;
            }
            faces.push(AggregatedGasFace {
                aperture,
                left_slot: left,
                right_slot: right,
            });
        }
        if volumes.iter().any(|v| !v.value().is_finite()) {
            return Err(fail("aggregated gas volume overflow"));
        }
        let centroids: Vec<[S; 3]> = moments
            .iter()
            .zip(&volumes)
            .map(|(m, v)| {
                if v.value() > 0. {
                    m.map(|x| x / *v)
                } else {
                    [S::zero(); 3]
                }
            })
            .collect();
        let mut second = vec![[[S::zero(); 3]; 3]; self.slots()];
        for (component, &slot) in p.components.iter().zip(&component_slots) {
            let delta: [S; 3] =
                std::array::from_fn(|k| component.centroid_m[k] - centroids[slot][k]);
            for j in 0..3 {
                for k in j..3 {
                    second[slot][j][k] += component.central_second_integral_m5[j][k]
                        + component.volume_m3 * delta[j] * delta[k];
                }
            }
        }
        for tensor in &mut second {
            for j in 0..3 {
                for k in j + 1..3 {
                    tensor[k][j] = tensor[j][k];
                }
            }
        }
        let covariance = second
            .into_iter()
            .zip(&volumes)
            .map(|(m, v)| {
                if v.value() > 0. {
                    let mut tensor = [[S::zero(); 3]; 3];
                    for j in 0..3 {
                        for k in j..3 {
                            tensor[j][k] = m[j][k] / *v;
                            tensor[k][j] = tensor[j][k];
                        }
                    }
                    Some(tensor)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if centroids.iter().flatten().any(|v| !v.value().is_finite())
            || covariance
                .iter()
                .flatten()
                .flat_map(|m| m.iter().flatten())
                .any(|v| !v.value().is_finite())
        {
            return Err(fail("aggregated gas volume moments overflow"));
        }
        Ok(AggregatedGasGeometry {
            central_covariance_m2: covariance,
            centroids_m: centroids,
            volumes_m3: volumes,
            active_slots: active,
            component_slots,
            faces,
        })
    }
    fn residual_partition<S: Scalar>(
        &self,
        p: &RefinedCutCellPartition<S>,
    ) -> NativeCutCellPartition<S> {
        let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
        let components = p
            .partition
            .components
            .iter()
            .zip(&p.regions)
            .filter_map(|(c, r)| {
                if r.is_some() {
                    return None;
                }
                let mut c = c.clone();
                let n = counts.entry(c.cell).or_default();
                c.component = *n;
                *n += 1;
                Some(c)
            })
            .collect::<Vec<_>>();
        NativeCutCellPartition {
            native_capacity_m3: p.partition.native_capacity_m3.clone(),
            total_volume_m3: components.iter().map(|c| c.volume_m3).fold(S::zero(), |a,b|a+b),
            components,
            apertures: vec![],
            walls: vec![],
            maximum_volume_defect_m3: p.partition.maximum_volume_defect_m3,
            arithmetic_length_m: p.partition.arithmetic_length_m,
        }
    }
    pub fn remap_sparse_coefficients<S: crate::gas_geometry_time::GeometryTimeBase>(
        &self,
        previous: &RefinedCutCellPartition<S>,
        current: &RefinedCutCellPartition<S>,
        inventory: &[Option<Vec<S>>],
        coefficient_count: usize,
        stationary_nonkinetic_slots: &[usize],
    ) -> CaeResult<SparseGasCoefficientRemap<S>> {
        if coefficient_count == 0
            || inventory.len() != self.slots()
            || inventory.iter().flatten().any(|row| {
                row.len() != coefficient_count
                    || row
                        .iter()
                        .any(|v| !v.finite_first_order() || v.value() < 0.)
            })
        {
            return Err(fail("invalid sparse gas coefficient inventory"));
        }
        let old = self.aggregate(previous)?;
        let geometry = self.aggregate(current)?;
        let mut excluded = std::collections::BTreeSet::new();
        for &slot in stationary_nonkinetic_slots {
            if slot >= self.slots() || !excluded.insert(slot) || inventory[slot].is_some() {
                return Err(fail("invalid nonkinetic gas slot ownership"));
            }
            let mut identity = None;
            for (partition, aggregated) in [(previous, &old), (current, &geometry)] {
                let indices: Vec<_> = aggregated
                    .component_slots
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &s)| (s == slot).then_some(i))
                    .collect();
                if indices.len() != 1 {
                    return Err(fail("nonkinetic slot is not one persistent physical cell"));
                }
                let index = indices[0];
                let component = &partition.partition.components[index];
                if partition.regions[index].is_some()
                    || component.convex_parts.len() != 1
                    || component.convex_parts[0].iter().any(|p| {
                        !matches!(
                            p.boundary,
                            crate::native_cut_cell_partition::CutBoundary::Cell { .. }
                        )
                    })
                    || component.volume_m3.has_direction()
                    || component.centroid_m.iter().any(|v| v.has_direction())
                {
                    return Err(fail(
                        "nonkinetic gas slot geometry is not stationary full fluid",
                    ));
                }
                let key = (component.cell, component.component);
                if identity.is_some_and(|v| v != key) {
                    return Err(fail("nonkinetic gas slot identity changes"));
                }
                identity = Some(key);
            }
            if old.volumes_m3[slot].value() != geometry.volumes_m3[slot].value()
                || old.centroids_m[slot].map(|v| v.value())
                    != geometry.centroids_m[slot].map(|v| v.value())
            {
                return Err(fail("nonkinetic gas capacity changes"));
            }
        }
        let mut output = vec![None; self.slots()];
        let mut transfers = Vec::new();
        for slot in 0..self.regions.len() {
            let row = inventory[slot].as_deref().unwrap_or(&[]);
            if (!old.active_slots[slot] || !geometry.active_slots[slot])
                && row.iter().any(|v| v.value() != 0.)
            {
                return Err(fail("inactive numerical gas region retains coefficients"));
            }
            if geometry.active_slots[slot] {
                if old.active_slots[slot] {
                    transfers.push(SparseGasCoefficientTransfer {
                        previous_slot: slot,
                        current_slot: slot,
                        weight: S::one(),
                    });
                }
                output[slot] = Some(if old.active_slots[slot] {
                    inventory[slot]
                        .clone()
                        .unwrap_or_else(|| vec![S::zero(); coefficient_count])
                } else {
                    vec![S::zero(); coefficient_count]
                });
            }
        }
        let before = self.residual_partition(previous);
        let after = self.residual_partition(current);
        let mut old_cells = BTreeMap::<usize, Vec<_>>::new();
        let mut new_cells = BTreeMap::<usize, Vec<_>>::new();
        for component in before.components {
            old_cells.entry(component.cell).or_default().push(component);
        }
        for component in after.components {
            new_cells.entry(component.cell).or_default().push(component);
        }
        let mut defect: f64 = 0.;
        let mut unrepresented: f64 = 0.;
        for (cell_index, &cell) in self.cells.iter().enumerate() {
            let base = self.regions.len() + cell_index * self.residual_slots_per_cell;
            let selected: Vec<_> = (base..base + self.residual_slots_per_cell)
                .filter(|s| excluded.contains(s))
                .collect();
            if !selected.is_empty() {
                if selected.len() != 1
                    || old_cells.get(&cell).map_or(0, Vec::len) != 1
                    || new_cells.get(&cell).map_or(0, Vec::len) != 1
                {
                    return Err(fail("nonkinetic cell contains another gas component"));
                }
                for slot in base..base + self.residual_slots_per_cell {
                    if inventory[slot]
                        .as_ref()
                        .is_some_and(|r| r.iter().any(|v| v.value() != 0.))
                    {
                        return Err(fail("nonkinetic cell also owns kinetic coefficients"));
                    }
                }
                continue;
            }
            let local = crate::native_cut_cell_partition::NativeCutCellLayout::new(
                vec![cell],
                self.residual_slots_per_cell,
                coefficient_count,
            )?;
            let partition = |components: Vec<_>, arithmetic_length_m: f64, capacity: S| {
                let total_volume_m3 = components
                    .iter()
                    .map(|c: &crate::native_cut_cell_partition::CutCellComponent<S>| c.volume_m3)
                    .fold(S::zero(), |a,b|a+b);
                crate::native_cut_cell_partition::NativeCutCellPartition {
                    components,
                    native_capacity_m3: BTreeMap::from([(cell, capacity)]),
                    total_volume_m3,
                    apertures: vec![],
                    walls: vec![],
                    maximum_volume_defect_m3: 0.,
                    arithmetic_length_m,
                }
            };
            let a = partition(
                old_cells.remove(&cell).unwrap_or_default(),
                previous.partition.arithmetic_length_m,
                *previous
                    .partition
                    .native_capacity_m3
                    .get(&cell)
                    .ok_or_else(|| fail("old native gas capacity metadata absent"))?,
            );
            let b = partition(
                new_cells.remove(&cell).unwrap_or_default(),
                current.partition.arithmetic_length_m,
                *current
                    .partition
                    .native_capacity_m3
                    .get(&cell)
                    .ok_or_else(|| fail("new native gas capacity metadata absent"))?,
            );
            let mut dense = vec![S::zero(); self.residual_slots_per_cell * coefficient_count];
            for k in 0..self.residual_slots_per_cell {
                if let Some(row) = &inventory[base + k] {
                    dense[k * coefficient_count..(k + 1) * coefficient_count].copy_from_slice(row);
                }
            }
            let remapped = local.remap(&a, &b, &dense)?;
            for (i, component) in a.components.iter().enumerate() {
                let total: S = remapped
                    .overlap
                    .iter()
                    .filter(|o| o.previous == i)
                    .map(|o| o.volume_m3)
                    .fold(S::zero(), |a,b|a+b);
                if total.value() == 0. {
                    continue;
                }
                for overlap in remapped.overlap.iter().filter(|o| o.previous == i) {
                    transfers.push(SparseGasCoefficientTransfer {
                        previous_slot: base + component.component,
                        current_slot: base + b.components[overlap.current].component,
                        weight: overlap.volume_m3 / total,
                    });
                }
            }
            defect = defect.max(remapped.maximum_population_balance_defect_kg);
            unrepresented = unrepresented.max(remapped.maximum_unrepresented_donor_volume_m3);
            for k in 0..self.residual_slots_per_cell {
                if remapped.active_slots[k] {
                    output[base + k] = Some(
                        remapped.inventories_kg[k * coefficient_count..(k + 1) * coefficient_count]
                            .to_vec(),
                    );
                }
            }
        }
        if output
            .iter()
            .flatten()
            .flatten()
            .any(|v| !v.finite_first_order())
        {
            return Err(fail("sparse gas coefficient remap overflow"));
        }
        for q in 0..coefficient_count {
            let before = inventory
                .iter()
                .flatten()
                .map(|r| r[q].value())
                .sum::<f64>();
            let after = output.iter().flatten().map(|r| r[q].value()).sum::<f64>();
            let error = (before - after).abs();
            if !after.is_finite()
                || error > 256. * f64::EPSILON * before.abs().max(f64::MIN_POSITIVE)
            {
                return Err(fail("sparse gas remap loses coefficient inventory"));
            }
            defect = defect.max(error);
        }
        Ok(SparseGasCoefficientRemap {
            transfers,
            coefficient_inventory: output,
            geometry,
            maximum_coefficient_balance_defect: defect,
            maximum_unrepresented_residual_volume_m3: unrepresented,
        })
    }
    pub fn remap_sparse_coefficients_transpose(
        &self,
        previous: &RefinedCutCellPartition<f64>,
        current: &RefinedCutCellPartition<f64>,
        inventory: &[Option<Vec<f64>>],
        coefficient_count: usize,
        stationary_nonkinetic_slots: &[usize],
        output_bars: &[Option<Vec<f64>>],
    ) -> CaeResult<SparseGasCoefficientTranspose> {
        let forward = self.remap_sparse_coefficients(
            previous,
            current,
            inventory,
            coefficient_count,
            stationary_nonkinetic_slots,
        )?;
        if output_bars.len() != self.slots()
            || output_bars
                .iter()
                .flatten()
                .any(|r| r.len() != coefficient_count || r.iter().any(|v| !v.is_finite()))
        {
            return Err(fail("invalid sparse gas remap cotangent"));
        }
        for (slot, row) in output_bars.iter().enumerate() {
            if forward.coefficient_inventory[slot].is_none()
                && row.as_ref().is_some_and(|r| r.iter().any(|v| *v != 0.))
            {
                return Err(fail("unowned sparse gas remap output cotangent"));
            }
        }
        let mut bars = inventory
            .iter()
            .map(|r| r.as_ref().map(|_| vec![0.; coefficient_count]))
            .collect::<Vec<_>>();
        let mut differentiable = std::collections::BTreeSet::new();
        for transfer in &forward.transfers {
            differentiable.insert(transfer.previous_slot);
            if let (Some(source), Some(target)) = (
                &mut bars[transfer.previous_slot],
                &output_bars[transfer.current_slot],
            ) {
                for (a, b) in source.iter_mut().zip(target) {
                    *a += transfer.weight * b;
                }
            }
        }
        if bars.iter().flatten().flatten().any(|v| !v.is_finite()) {
            return Err(fail("sparse gas remap cotangent overflow"));
        }
        let fixed_zero_previous_slots = inventory
            .iter()
            .enumerate()
            .filter_map(|(slot, r)| {
                (r.is_some() && !differentiable.contains(&slot)).then_some(slot)
            })
            .collect();
        Ok(SparseGasCoefficientTranspose {
            coefficient_inventory_bars: bars,
            fixed_zero_previous_slots,
        })
    }
    pub fn remap<S: Scalar>(
        &self,
        previous: &RefinedCutCellPartition<S>,
        current: &RefinedCutCellPartition<S>,
        inventory: &[S],
    ) -> CaeResult<AggregatedGasInventoryRemap<S>> {
        if inventory.len() != self.state_size
            || inventory
                .iter()
                .any(|v| !v.value().is_finite() || v.value() < 0.)
        {
            return Err(fail("invalid aggregated gas inventory"));
        }
        let old = self.aggregate(previous)?;
        let geometry = self.aggregate(current)?;
        let prefix = self.regions.len() * self.populations;
        let mut out = vec![S::zero(); self.state_size];
        for slot in 0..self.regions.len() {
            let row = &inventory[slot * self.populations..(slot + 1) * self.populations];
            if !old.active_slots[slot] && row.iter().any(|v| v.value() != 0.) {
                return Err(fail("inactive numerical gas region contains inventory"));
            }
            if old.active_slots[slot]
                && !geometry.active_slots[slot]
                && row.iter().any(|v| v.value() > 0.)
            {
                return Err(fail("disappearing numerical gas region retains inventory"));
            }
            if old.active_slots[slot] && geometry.active_slots[slot] {
                out[slot * self.populations..(slot + 1) * self.populations].copy_from_slice(row);
            }
        }
        let residual = self.residual.remap(
            &self.residual_partition(previous),
            &self.residual_partition(current),
            &inventory[prefix..],
        )?;
        out[prefix..].copy_from_slice(&residual.inventories_kg);
        let mut defect: f64 = 0.;
        for q in 0..self.populations {
            let before = (0..self.slots())
                .map(|s| inventory[s * self.populations + q].value())
                .sum::<f64>();
            let after = (0..self.slots())
                .map(|s| out[s * self.populations + q].value())
                .sum::<f64>();
            let e = (before - after).abs();
            defect = defect.max(e);
            if !after.is_finite() || e > 256. * f64::EPSILON * before.abs().max(f64::MIN_POSITIVE) {
                return Err(fail("aggregated gas remap loses population inventory"));
            }
        }
        Ok(AggregatedGasInventoryRemap {
            inventories_kg: out,
            geometry,
            maximum_population_balance_defect_kg: defect,
            maximum_unrepresented_residual_volume_m3: residual
                .maximum_unrepresented_donor_volume_m3,
        })
    }
}
