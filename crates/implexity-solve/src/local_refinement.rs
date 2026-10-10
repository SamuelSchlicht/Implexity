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



use implexity_core::error::{CaeError, CaeResult};
use ndarray::{Array3, ArrayView3};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefinementRequirement {
    pub feature_m: f64,
    pub cells_per_feature: u32,
    pub max_levels: u32,
}

impl RefinementRequirement {
    #[must_use]
    pub fn new(feature_m: f64) -> Self {
        Self { feature_m, cells_per_feature: 4, max_levels: 8 }
    }

    #[must_use]
    pub fn target_cell_m(&self) -> f64 {
        self.feature_m / f64::from(self.cells_per_feature)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefinementOptions {
    pub interface_band: f64,
    pub feature_band: f64,
    pub hotspot_axis_fraction: f64,
    pub active_mask: Option<Array3<bool>>,
    pub axial_slab_cells: usize,
}

impl Default for RefinementOptions {
    fn default() -> Self {
        Self {
            interface_band: 0.18,
            feature_band: 0.08,
            hotspot_axis_fraction: 0.65,
            active_mask: None,
            axial_slab_cells: 8,
        }
    }
}

fn slab_boxes(mask: &ArrayView3<'_, bool>, step: usize, pad: usize) -> Vec<Value> {
    let (nx, ny, nz) = mask.dim();
    let step = step.max(1);
    let mut boxes = Vec::new();
    let mut x0 = 0;
    while x0 < nx {
        let x1 = nx.min(x0 + step);
        let mut bounds: Option<(usize, usize, usize, usize)> = None;
        for x in x0..x1 {
            for y in 0..ny {
                for z in 0..nz {
                    if mask[[x, y, z]] {
                        bounds = Some(match bounds {
                            None => (y, y, z, z),
                            Some((a, b, c, d)) => (a.min(y), b.max(y), c.min(z), d.max(z)),
                        });
                    }
                }
            }
        }
        if let Some((ya, yb, za, zb)) = bounds {
            let (y0, y1) = (ya.saturating_sub(pad), ny.min(yb + pad + 1));
            let (z0, z1) = (za.saturating_sub(pad), nz.min(zb + pad + 1));
            boxes.push(json!([[x0, x1], [y0, y1], [z0, z1]]));
        }
        x0 += step;
    }
    boxes
}



pub fn plan_local_refinement(
    topology: &Array3<f64>,
    feature_logits: &Array3<f64>,
    dimensions_m: [f64; 3],
    requirement: &RefinementRequirement,
    options: &RefinementOptions,
) -> CaeResult<Value> {
    if feature_logits.dim() != topology.dim() {
        return Err(CaeError::contract("feature_logits shape must match topology"));
    }
    let shape = topology.shape();
    #[allow(clippy::cast_precision_loss)]
    let base: Vec<f64> = (0..3).map(|a| dimensions_m[a] / shape[a] as f64).collect();
    let target = requirement.target_cell_m();
    let ratio = (base.iter().copied().fold(f64::NEG_INFINITY, f64::max) / target).max(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let levels = (ratio.log2().ceil().max(0.0) as u32).min(requirement.max_levels);
    let mut interface = topology.mapv(|t| t > options.interface_band && t < 1.0 - options.interface_band);
    let mut feature = feature_logits.mapv(|c| {
        let p = 1.0 / (1.0 + (-c).exp());
        p > options.feature_band && p < 1.0 - options.feature_band
    });
    if let Some(active) = &options.active_mask {
        if active.dim() != topology.dim() {
            return Err(CaeError::contract("active_mask shape must match topology"));
        }
        interface.zip_mut_with(active, |a, b| *a &= *b);
        feature.zip_mut_with(active, |a, b| *a &= *b);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let hot_start = (options.hotspot_axis_fraction * shape[0] as f64) as usize;
    let hot = Array3::from_shape_fn(topology.dim(), |(x, y, z)| x >= hot_start && topology[[x, y, z]] > 0.1);
    let step = options.axial_slab_cells;
    let finest: Vec<f64> = base.iter().map(|b| b / f64::from(1u32 << levels.min(31))).collect();
    let tol = target * (1.0 + 1e-12);
    Ok(json!({
        "base_cell_m": base,
        "feature_m": requirement.feature_m,
        "cells_per_feature": requirement.cells_per_feature,
        "target_cell_m": target,
        "required_levels": levels,
        "planned_finest_cell_m": finest,
        "resolution_gate_passes_if_solved_at_planned_level": finest.iter().all(|f| *f <= tol),
        "currently_verified": base.iter().all(|b| *b <= tol),
        "patches": {
            "solid_void_interface": slab_boxes(&interface.view(), step, 3),
            "secondary_feature_interface": slab_boxes(&feature.view(), step, 3),
            "downstream_hot_region": slab_boxes(&hot.view(), step, 2),
        },
        "warning": "Interpolation of the coarse field does not satisfy this gate; physics/geometry must be re-evaluated on the refined cells.",
    }))
}

