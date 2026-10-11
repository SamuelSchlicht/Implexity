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

use crate::gas_transient_kinetic::TransientGasScalar;
use crate::native_cut_cell_partition::NativeCutCellPartition;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

pub(crate) struct BoundaryMomentSample<S: Scalar> {
    pub component: usize,
    pub position_m: [S; 3],
    pub local_position_m: Option<([f64; 3], [S; 3])>,
    pub normal: [S; 3],
    pub area_m2: S,
}
fn fail(s: &str) -> CaeError { CaeError::contract(s) }
fn point_in_frame<S: Scalar>(q: &BoundaryMomentSample<S>, frame: [f64; 3]) -> [S; 3] {
    match q.local_position_m {
        Some((anchor, point)) => std::array::from_fn(|k| point[k] + (anchor[k] - frame[k])),
        None => std::array::from_fn(|k| q.position_m[k] - frame[k]),
    }
}
pub(crate) fn update<S: TransientGasScalar>(partition: &mut NativeCutCellPartition<S>, samples: &[BoundaryMomentSample<S>]) -> CaeResult<()> {
    let frames = partition.components.iter().map(|c| c.local_centroid_m.map_or([0.; 3], |(a, _)| a)).collect::<Vec<_>>();
    let origins = partition.components.iter().zip(&frames).map(|(c, frame)| {
        match c.local_centroid_m {
            Some((anchor, point)) => std::array::from_fn(|k| S::from_f64(point[k].value() + (anchor[k] - frame[k]))),
            None => std::array::from_fn(|k| S::from_f64(c.centroid_m[k].value() - frame[k])),
        }
    }).collect::<Vec<[S; 3]>>();
    let count = frames.len();
    let mut volume = vec![S::zero(); count];
    let mut moment = vec![[S::zero(); 3]; count];
    let mut moment_scale = vec![[0.; 3]; count];
    let mut closure = vec![[S::zero(); 3]; count];
    let mut area_scale = vec![0.; count];
    let mut volume_scale = vec![0.; count];
    for q in samples {
        let frame = *frames.get(q.component).ok_or_else(|| fail("boundary moment component is absent"))?;
        let point = point_in_frame(q, frame);
        let x: [S; 3] = std::array::from_fn(|k| point[k] - origins[q.component][k]);
        if !q.area_m2.finite_first_order() || q.area_m2.value() <= 0. || x.iter().chain(q.normal.iter()).any(|v| !v.finite_first_order()) {
            return Err(fail("boundary moment sample is invalid"));
        }
        let flux = (0..3).map(|k| x[k] * q.normal[k]).fold(S::zero(), |a, b| a + b) * q.area_m2;
        volume[q.component] += flux / 3.;
        volume_scale[q.component] += flux.value().abs() / 3.;
        area_scale[q.component] += q.area_m2.value();
        for k in 0..3 {
            let contribution = x[k] * flux / 4.;
            moment[q.component][k] += contribution;
            moment_scale[q.component][k] += contribution.value().abs();
            closure[q.component][k] += q.normal[k] * q.area_m2;
        }
    }
    let mut centres = Vec::with_capacity(count);
    for (i, c) in partition.components.iter().enumerate() {
        if !c.volume_m3.value().is_finite() || c.volume_m3.value() <= 0. || c.central_second_integral_m5.iter().flatten().any(|v| !v.value().is_finite()) {
            return Err(fail(&format!("boundary moment reference is invalid: component={i}, cell={}", c.cell)));
        }
        let bound = 4096. * f64::EPSILON * (volume_scale[i] + c.volume_m3.value().abs());
        let closure_bound = 4096. * f64::EPSILON * area_scale[i];
        if !volume[i].finite_first_order() || volume[i].value() <= 0. || (volume[i].value() - c.volume_m3.value()).abs() > bound || closure[i].iter().any(|v| !v.finite_first_order() || v.value().abs() > closure_bound) {
            return Err(fail(&format!("boundary moments do not close: component={i}, cell={}, volume={}, reference={}, volume_bound={bound}, closure={:?}, closure_bound={closure_bound}", c.cell, volume[i].value(), c.volume_m3.value(), closure[i].map(|v|v.value()))));
        }
        let original_centre: [S; 3] = match c.local_centroid_m { Some((anchor, point))=>std::array::from_fn(|k|point[k]+(anchor[k]-frames[i][k])), None=>std::array::from_fn(|k|c.centroid_m[k]-frames[i][k]) };
        for k in 0..3 {
            let original = (original_centre[k].value() - origins[i][k].value()) * c.volume_m3.value();
            let bound = 4096. * f64::EPSILON * (moment_scale[i][k] + original.abs());
            if (moment[i][k].value() - original).abs() > bound { return Err(fail(&format!("boundary first moment differs: component={i}, cell={}, axis={k}, computed={}, reference={original}, bound={bound}", c.cell, moment[i][k].value()))); }
        }
        centres.push(moment[i].map(|v| v / volume[i]));
    }
    let mut second = vec![[[S::zero(); 3]; 3]; count];
    let mut second_scale = vec![[[0.; 3]; 3]; count];
    for q in samples {
        let x = point_in_frame(q, frames[q.component]);
        let y: [S; 3] = std::array::from_fn(|k| (x[k] - origins[q.component][k]) - centres[q.component][k]);
        let flux = (0..3).map(|k| y[k] * q.normal[k]).fold(S::zero(), |a, b| a + b) * q.area_m2 / 5.;
        for j in 0..3 { for k in 0..3 {
            let contribution = y[j] * y[k] * flux;
            second[q.component][j][k] += contribution;
            second_scale[q.component][j][k] += contribution.value().abs();
        } }
    }
    for (i, c) in partition.components.iter_mut().enumerate() {
        if centres[i].iter().chain(second[i].iter().flatten()).any(|v| !v.finite_first_order()) { return Err(fail("boundary moments are not finite")); }
        for j in 0..3 { for k in 0..3 {
            let original = c.central_second_integral_m5[j][k].value();
            let bound = 4096. * f64::EPSILON * (second_scale[i][j][k] + original.abs());
            if (second[i][j][k].value() - original).abs() > bound { return Err(fail(&format!("boundary second moment differs: component={i}, cell={}, row={j}, column={k}, computed={}, reference={original}, bound={bound}", c.cell, second[i][j][k].value()))); }
        } }
        c.volume_m3 = volume[i];
        let local_centre: [S; 3] = std::array::from_fn(|k| centres[i][k] + origins[i][k]);
        c.local_centroid_m = Some((frames[i], local_centre));
        c.centroid_m = std::array::from_fn(|k| local_centre[k] + frames[i][k]);
        c.central_second_integral_m5 = second[i];
    }
    partition.total_volume_m3 = volume.into_iter().fold(S::zero(), |a, b| a + b);
    Ok(())
}
