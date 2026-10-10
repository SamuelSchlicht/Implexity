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
    contact_constraint_tangent::{
        ContactOriginSelection, ContactTangentPolicy, ContactTraceExtension,
    },
    moving_contact::contact_field::{ContactTraceConstraintPair, NativeContactLaw},
};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::multirate_coupling::{TraceDerivativeExtension, TraceExtensionCotangent};
use std::sync::Arc;

#[derive(Clone)]
pub struct NativeClosedGeometryWitness {
    kinematic_identity: String,
    trace_map: Vec<usize>,
    design_map: Vec<usize>,
    start: Vec<u64>,
    end: Vec<u64>,
    design: Vec<u64>,
    n: usize,
    time_scale: u64,
    closed_start_charts: Vec<usize>,
    closed_end_charts: Vec<usize>,
    macro_parent: Option<Arc<NativeClosedGeometryWitness>>,
}
impl NativeClosedGeometryWitness {
    pub fn validate_time_scale(&self, time_scale: f64) -> CaeResult<()> {
        if time_scale.to_bits() != self.time_scale {
            return Err(CaeError::contract(
                "closed primal geometry witness time scale",
            ));
        }
        Ok(())
    }
    pub fn from_patch_trace(
        law: &crate::patch_unilateral_contact::PatchUnilateralLaw,
        n: usize,
        start: &[f64],
        end: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<Self> {
        if !p.time_scale.is_finite() || p.time_scale <= 0. {
            return Err(CaeError::contract("closed geometry witness time scale"));
        }
        let a = law.release_trace_chart_gaps(start, p.design)?;
        let b = law.release_trace_chart_gaps(end, p.design)?;
        if a.len() != b.len()
            || a.iter()
                .chain(&b)
                .flatten()
                .any(|v| !v.is_finite() || *v < 0.)
        {
            return Err(CaeError::contract(
                "closed geometry witness nonnegative chart domain",
            ));
        }
        let closed = |g: &Vec<[f64; 3]>| {
            g.iter()
                .enumerate()
                .filter_map(|(i, v)| v.iter().all(|x| *x == 0.).then_some(i))
                .collect::<Vec<_>>()
        };
        let (binding, _, trace_map, design_map, _) = law.release_binding();
        Ok(Self {
            kinematic_identity: binding.binding_identity(),
            trace_map: trace_map.to_vec(),
            design_map: design_map.to_vec(),
            start: start.iter().map(|v| v.to_bits()).collect(),
            end: end.iter().map(|v| v.to_bits()).collect(),
            design: p.design.iter().map(|v| v.to_bits()).collect(),
            n,
            time_scale: p.time_scale.to_bits(),
            closed_start_charts: closed(&a),
            closed_end_charts: closed(&b),
            macro_parent: None,
        })
    }
    pub fn sample_child(
        self: &Arc<Self>,
        law: &crate::patch_unilateral_contact::PatchUnilateralLaw,
        alpha: f64,
        beta: f64,
    ) -> CaeResult<Self> {
        if self.macro_parent.is_some() {
            return Err(CaeError::contract(
                "closed witness child must sample its original macro owner",
            ));
        }
        check_sample_fractions(alpha, beta)?;
        check_patch_binding(
            law,
            &self.kinematic_identity,
            &self.trace_map,
            &self.design_map,
        )?;
        let start = sample_bits(&self.start, &self.end, alpha)?;
        let end = sample_bits(&self.start, &self.end, beta)?;
        let design = decode_bits(&self.design);
        let p = implexity_solve::time_stepper::StepParameters {
            design: &design,
            time_scale: f64::from_bits(self.time_scale),
        };
        let mut child = Self::from_patch_trace(law, self.n, &start, &end, p)?;
        child.closed_start_charts = selected_sample_charts(
            &child.closed_start_charts,
            &self.closed_start_charts,
            &self.closed_end_charts,
            alpha,
        );
        child.closed_end_charts = selected_sample_charts(
            &child.closed_end_charts,
            &self.closed_start_charts,
            &self.closed_end_charts,
            beta,
        );
        child.macro_parent = Some(self.clone());
        Ok(child)
    }
    pub fn macro_witness(&self) -> &Self {
        self.macro_parent.as_deref().unwrap_or(self)
    }
    pub fn closed_start_charts(&self) -> &[usize] {
        &self.closed_start_charts
    }
    pub fn closed_end_charts(&self) -> &[usize] {
        &self.closed_end_charts
    }
    pub fn validate_macro(
        &self,
        n: usize,
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<()> {
        if n != self.n
            || p.time_scale.to_bits() != self.time_scale
            || p.design.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.design
        {
            return Err(CaeError::contract(
                "closed primal geometry witness macro identity",
            ));
        }
        Ok(())
    }
    pub fn validate_path(
        &self,
        kinematics: &crate::surface_gap::SurfaceGapKinematics,
        start: &[f64],
        end: &[f64],
        design: &[f64],
    ) -> CaeResult<()> {
        let (binding, trace_map, design_map) = kinematics.as_trace_kinematics()?.compact()?;
        if binding.binding_identity() != self.kinematic_identity
            || trace_map != self.trace_map
            || design_map != self.design_map
            || start.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.start
            || end.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.end
            || design.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.design
        {
            return Err(CaeError::contract(
                "closed primal geometry witness does not own this path",
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct NativeClosedReleaseCertificate {
    replay_identity: String,
    extension: Arc<ContactTraceExtension>,
    compatibility_relative_tolerance: f64,
    kinematic_identity: String,
    trace_map: Vec<usize>,
    design_map: Vec<usize>,
    start: Vec<u64>,
    end: Vec<u64>,
    design: Vec<u64>,
    closed_endpoint_corners: Vec<(usize, usize)>,
    released_corners: Vec<(usize, usize)>,
    closed_start_charts: Vec<usize>,
    released_charts: Vec<usize>,
    n: usize,
    time_scale: u64,
}

impl NativeClosedReleaseCertificate {
    pub fn validate_time_scale(&self, time_scale: f64) -> CaeResult<()> {
        if time_scale.to_bits() != self.time_scale {
            return Err(CaeError::contract("closed release certificate time scale"));
        }
        Ok(())
    }
    pub fn validate_macro(
        &self,
        n: usize,
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<()> {
        if n != self.n
            || p.time_scale.to_bits() != self.time_scale
            || p.design.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.design
        {
            return Err(CaeError::contract(
                "closed release certificate macro identity",
            ));
        }
        Ok(())
    }
    pub fn sample_child(
        self: &Arc<Self>,
        law: &crate::patch_unilateral_contact::PatchUnilateralLaw,
        alpha: f64,
        beta: f64,
    ) -> CaeResult<NativeClosedReleaseSample> {
        check_sample_fractions(alpha, beta)?;
        check_patch_binding(
            law,
            &self.kinematic_identity,
            &self.trace_map,
            &self.design_map,
        )?;
        let start = sample_bits(&self.start, &self.end, alpha)?;
        let end = sample_bits(&self.start, &self.end, beta)?;
        let design = decode_bits(&self.design);
        let a = law.release_trace_chart_gaps(&start, &design)?;
        let b = law.release_trace_chart_gaps(&end, &design)?;
        if a.len() != b.len()
            || a.iter()
                .chain(&b)
                .flatten()
                .any(|v| !v.is_finite() || *v < 0.)
        {
            return Err(CaeError::contract("closed release sampled chart domain"));
        }
        let closed = |g: &Vec<[f64; 3]>| {
            g.iter()
                .enumerate()
                .filter_map(|(i, v)| v.iter().all(|x| *x == 0.).then_some(i))
                .collect::<Vec<_>>()
        };
        let end_closed: Vec<_> = self
            .closed_start_charts
            .iter()
            .copied()
            .filter(|chart| {
                (0..3).all(|corner| self.closed_endpoint_corners.contains(&(*chart, corner)))
            })
            .collect();
        Ok(NativeClosedReleaseSample {
            parent: self.clone(),
            alpha,
            beta,
            start: start.iter().map(|v| v.to_bits()).collect(),
            end: end.iter().map(|v| v.to_bits()).collect(),
            closed_start_charts: selected_sample_charts(
                &closed(&a),
                &self.closed_start_charts,
                &end_closed,
                alpha,
            ),
            closed_end_charts: selected_sample_charts(
                &closed(&b),
                &self.closed_start_charts,
                &end_closed,
                beta,
            ),
        })
    }
    pub fn closed_endpoint_corners(&self) -> &[(usize, usize)] {
        &self.closed_endpoint_corners
    }
    pub fn released_corners(&self) -> &[(usize, usize)] {
        &self.released_corners
    }
    pub fn closed_start_charts(&self) -> &[usize] {
        &self.closed_start_charts
    }
    pub fn released_charts(&self) -> &[usize] {
        &self.released_charts
    }
    pub fn project_direction(
        &self,
        start: &[f64],
        end: &[f64],
        design: Option<&[f64]>,
        time: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.extension.tangent(start, end, design, time)
    }
    pub fn validate_direction(
        &self,
        start: &[f64],
        end: &[f64],
        design: Option<&[f64]>,
        time: f64,
    ) -> CaeResult<()> {
        let (a, b) = self.extension.tangent(start, end, design, time)?;
        for (supplied, selected) in [(start, a.as_slice()), (end, b.as_slice())] {
            if supplied.len() != selected.len() {
                return Err(CaeError::contract("closed release direction shape"));
            }
            let scale = supplied
                .iter()
                .chain(selected)
                .map(|v| v.abs())
                .fold(0., f64::max);
            let defect = supplied
                .iter()
                .zip(selected)
                .map(|(x, y)| (x - y).abs())
                .fold(0., f64::max);
            if defect > self.compatibility_relative_tolerance * scale {
                return Err(CaeError::contract(
                    "closed release direction leaves the selected contact manifold",
                ));
            }
        }
        Ok(())
    }
    pub fn validate_path(
        &self,
        kinematics: &crate::surface_gap::SurfaceGapKinematics,
        start: &[f64],
        end: &[f64],
        design: &[f64],
    ) -> CaeResult<()> {
        let (binding, trace_map, design_map) = kinematics.as_trace_kinematics()?.compact()?;
        if binding.binding_identity() != self.kinematic_identity
            || trace_map != self.trace_map
            || design_map != self.design_map
            || start.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.start
            || end.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.end
            || design.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.design
        {
            return Err(CaeError::contract(
                "closed release certificate does not own this geometry path",
            ));
        }
        Ok(())
    }
}

pub struct NativeClosedReleaseSample {
    parent: Arc<NativeClosedReleaseCertificate>,
    alpha: f64,
    beta: f64,
    start: Vec<u64>,
    end: Vec<u64>,
    closed_start_charts: Vec<usize>,
    closed_end_charts: Vec<usize>,
}
impl NativeClosedReleaseSample {
    pub fn parent(&self) -> &NativeClosedReleaseCertificate {
        &self.parent
    }
    pub fn fractions(&self) -> (f64, f64) {
        (self.alpha, self.beta)
    }
    pub fn closed_start_charts(&self) -> &[usize] {
        &self.closed_start_charts
    }
    pub fn closed_end_charts(&self) -> &[usize] {
        &self.closed_end_charts
    }
    pub fn validate_path(
        &self,
        kinematics: &crate::surface_gap::SurfaceGapKinematics,
        macro_start: &[f64],
        macro_end: &[f64],
        start: &[f64],
        end: &[f64],
        design: &[f64],
    ) -> CaeResult<()> {
        self.parent
            .validate_path(kinematics, macro_start, macro_end, design)?;
        if start.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.start
            || end.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != self.end
        {
            return Err(CaeError::contract(
                "closed release sample does not own these child endpoints",
            ));
        }
        Ok(())
    }
    pub fn project_macro_direction(
        &self,
        start: &[f64],
        end: &[f64],
        design: Option<&[f64]>,
        time: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.parent.project_direction(start, end, design, time)
    }
    pub fn project_sampled_direction(
        &self,
        start: &[f64],
        end: &[f64],
        design: Option<&[f64]>,
        time: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let (a, b) = self.project_macro_direction(start, end, design, time)?;
        Ok((
            sample_values(&a, &b, self.alpha)?,
            sample_values(&a, &b, self.beta)?,
        ))
    }
}
fn check_sample_fractions(alpha: f64, beta: f64) -> CaeResult<()> {
    if !alpha.is_finite() || !beta.is_finite() || alpha < 0. || alpha >= beta || beta > 1. {
        return Err(CaeError::contract("closed geometry sample fractions"));
    }
    Ok(())
}
fn decode_bits(values: &[u64]) -> Vec<f64> {
    values.iter().map(|v| f64::from_bits(*v)).collect()
}
fn sample_bits(start: &[u64], end: &[u64], fraction: f64) -> CaeResult<Vec<f64>> {
    sample_values(&decode_bits(start), &decode_bits(end), fraction)
}
fn sample_values(start: &[f64], end: &[f64], fraction: f64) -> CaeResult<Vec<f64>> {
    if start.len() != end.len() || start.iter().chain(end).any(|v| !v.is_finite()) {
        return Err(CaeError::contract("closed geometry sample trace shape"));
    }
    let result: Vec<_> = start
        .iter()
        .zip(end)
        .map(|(s, e)| s + (e - s) * fraction)
        .collect();
    if result.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract("closed geometry sampled trace overflow"));
    }
    Ok(result)
}
fn selected_sample_charts(
    actual: &[usize],
    start: &[usize],
    end: &[usize],
    fraction: f64,
) -> Vec<usize> {
    actual
        .iter()
        .copied()
        .filter(|chart| {
            if fraction == 0. {
                start.contains(chart)
            } else if fraction == 1. {
                end.contains(chart)
            } else {
                start.contains(chart) && end.contains(chart)
            }
        })
        .collect()
}
fn check_patch_binding(
    law: &crate::patch_unilateral_contact::PatchUnilateralLaw,
    identity: &str,
    trace: &[usize],
    design: &[usize],
) -> CaeResult<()> {
    let (binding, _, tm, dm, _) = law.release_binding();
    if binding.binding_identity() != identity || tm != trace || dm != design {
        return Err(CaeError::contract("closed sample patch ownership"));
    }
    Ok(())
}

mod projector_seal {
    pub trait Sealed {}
}
pub(crate) trait NativeClosedGeometryProjector: projector_seal::Sealed {
    fn validate_path(
        &self,
        kinematics: &crate::surface_gap::SurfaceGapKinematics,
        start: &[f64],
        end: &[f64],
        design: &[f64],
    ) -> CaeResult<()>;
    fn validate_time_scale(&self, time_scale: f64) -> CaeResult<()>;
    fn project_direction(
        &self,
        start: &[f64],
        end: &[f64],
        design: Option<&[f64]>,
        time: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)>;
}
impl projector_seal::Sealed for NativeClosedReleaseCertificate {}
impl NativeClosedGeometryProjector for NativeClosedReleaseCertificate {
    fn validate_path(
        &self,
        k: &crate::surface_gap::SurfaceGapKinematics,
        a: &[f64],
        b: &[f64],
        x: &[f64],
    ) -> CaeResult<()> {
        NativeClosedReleaseCertificate::validate_path(self, k, a, b, x)
    }
    fn validate_time_scale(&self, t: f64) -> CaeResult<()> {
        NativeClosedReleaseCertificate::validate_time_scale(self, t)
    }
    fn project_direction(
        &self,
        a: &[f64],
        b: &[f64],
        x: Option<&[f64]>,
        t: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        NativeClosedReleaseCertificate::project_direction(self, a, b, x, t)
    }
}
#[derive(Clone)]
pub struct NativeClosedTrialCertificate {
    witness: Arc<NativeClosedGeometryWitness>,
    extension: Arc<ContactTraceExtension>,
}
impl NativeClosedTrialCertificate {
    pub fn validate_macro(
        &self,
        n: usize,
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<()> {
        self.witness.validate_macro(n, p)
    }
    pub fn validate_time_scale(&self, t: f64) -> CaeResult<()> {
        self.witness.validate_time_scale(t)
    }
    pub fn validate_path(
        &self,
        k: &crate::surface_gap::SurfaceGapKinematics,
        a: &[f64],
        b: &[f64],
        x: &[f64],
    ) -> CaeResult<()> {
        self.witness.validate_path(k, a, b, x)
    }
    pub fn project_direction(
        &self,
        a: &[f64],
        b: &[f64],
        x: Option<&[f64]>,
        t: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.extension.tangent(a, b, x, t)
    }
    pub fn sample_child(
        self: &Arc<Self>,
        law: &crate::patch_unilateral_contact::PatchUnilateralLaw,
        alpha: f64,
        beta: f64,
    ) -> CaeResult<NativeClosedTrialSample> {
        Ok(NativeClosedTrialSample {
            parent: self.clone(),
            alpha,
            beta,
            witness: self.witness.sample_child(law, alpha, beta)?,
        })
    }
}
impl projector_seal::Sealed for NativeClosedTrialCertificate {}
impl NativeClosedGeometryProjector for NativeClosedTrialCertificate {
    fn validate_path(
        &self,
        k: &crate::surface_gap::SurfaceGapKinematics,
        a: &[f64],
        b: &[f64],
        x: &[f64],
    ) -> CaeResult<()> {
        NativeClosedTrialCertificate::validate_path(self, k, a, b, x)
    }
    fn validate_time_scale(&self, t: f64) -> CaeResult<()> {
        NativeClosedTrialCertificate::validate_time_scale(self, t)
    }
    fn project_direction(
        &self,
        a: &[f64],
        b: &[f64],
        x: Option<&[f64]>,
        t: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        NativeClosedTrialCertificate::project_direction(self, a, b, x, t)
    }
}
pub struct NativeClosedTrialSample {
    parent: Arc<NativeClosedTrialCertificate>,
    alpha: f64,
    beta: f64,
    witness: NativeClosedGeometryWitness,
}
impl NativeClosedTrialSample {
    pub fn parent(&self) -> &NativeClosedTrialCertificate {
        &self.parent
    }
    pub fn fractions(&self) -> (f64, f64) {
        (self.alpha, self.beta)
    }
    pub fn closed_start_charts(&self) -> &[usize] {
        self.witness.closed_start_charts()
    }
    pub fn closed_end_charts(&self) -> &[usize] {
        self.witness.closed_end_charts()
    }
    pub fn validate_path(
        &self,
        k: &crate::surface_gap::SurfaceGapKinematics,
        ma: &[f64],
        mb: &[f64],
        a: &[f64],
        b: &[f64],
        x: &[f64],
    ) -> CaeResult<()> {
        self.parent.validate_path(k, ma, mb, x)?;
        self.witness.validate_path(k, a, b, x)
    }
    pub fn project_macro_direction(
        &self,
        a: &[f64],
        b: &[f64],
        x: Option<&[f64]>,
        t: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.parent.project_direction(a, b, x, t)
    }
}
pub struct NativeClosedTrialTrace {
    extension: Arc<ContactTraceExtension>,
    certificate: Arc<NativeClosedTrialCertificate>,
}
impl TraceDerivativeExtension for NativeClosedTrialTrace {
    fn tangent(
        &self,
        a: &[f64],
        b: &[f64],
        x: Option<&[f64]>,
        t: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.extension.tangent(a, b, x, t)
    }
    fn adjoint(&self, a: &[f64], b: &[f64]) -> CaeResult<TraceExtensionCotangent> {
        self.extension.adjoint(a, b)
    }
    fn derivative_context(
        &self,
    ) -> Option<Arc<implexity_solve::multirate_coupling::SubcycleDerivativeContext>> {
        Some(self.certificate.clone())
    }
}

pub struct NativeClosedReleaseTrace {
    extension: Arc<ContactTraceExtension>,
    released_rows: Vec<usize>,
    maintained_origin_rows: Vec<usize>,
    certificate: Option<Arc<NativeClosedReleaseCertificate>>,
}

impl NativeClosedReleaseTrace {
    pub fn new(
        trace: &CsrMatrix,
        data: ContactTraceConstraintPair,
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        Self::new_mode(trace, data, policy, false)
    }
    fn new_mode(
        trace: &CsrMatrix,
        mut data: ContactTraceConstraintPair,
        policy: ContactTangentPolicy,
        maintained: bool,
    ) -> CaeResult<Self> {
        if data.start.branches.len() != data.end.branches.len() {
            return Err(CaeError::contract("release contact endpoint row mismatch"));
        }
        let mut released_rows = Vec::new();
        let mut maintained_origin_rows = Vec::new();
        for (i, (start, end)) in data
            .start
            .branches
            .iter()
            .zip(&data.end.branches)
            .enumerate()
        {
            if start.gap_m != 0. {
                continue;
            }
            if start.multiplier_n == 0. {
                if data
                    .start
                    .origins
                    .get(&i)
                    .is_some_and(|v| *v != ContactOriginSelection::Closed)
                {
                    return Err(CaeError::contract(
                        "release start must retain the closed contact manifold",
                    ));
                }
                data.start.origins.insert(i, ContactOriginSelection::Closed);
            }
            if end.gap_m > 0. && end.multiplier_n == 0. {
                released_rows.push(i);
            } else if end.gap_m == 0. && end.multiplier_n == 0. {
                match data.end.origins.get(&i) {
                    Some(ContactOriginSelection::Closed) => maintained_origin_rows.push(i),
                    _ => {
                        return Err(CaeError::contract(
                            "release endpoint origin requires a separately selected closed branch",
                        ));
                    }
                }
            }
        }
        if released_rows.is_empty() && !maintained {
            return Err(CaeError::contract(
                "finite closed release needs a strictly open endpoint",
            ));
        }
        let extension = Arc::new(ContactTraceExtension::new(trace, data, policy)?);
        Ok(Self {
            extension,
            released_rows,
            maintained_origin_rows,
            certificate: None,
        })
    }

    pub fn from_patch(
        law: &crate::patch_unilateral_contact::PatchUnilateralLaw,
        trace: &CsrMatrix,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        Self::from_patch_with_endpoint_origins(
            law,
            trace,
            n,
            current,
            previous,
            p,
            policy,
            &std::collections::BTreeMap::new(),
        )
    }

    pub fn from_patch_with_endpoint_origins(
        law: &crate::patch_unilateral_contact::PatchUnilateralLaw,
        trace: &CsrMatrix,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        policy: ContactTangentPolicy,
        endpoint_origins: &std::collections::BTreeMap<usize, ContactOriginSelection>,
    ) -> CaeResult<Self> {
        Self::from_patch_mode(
            law,
            trace,
            n,
            current,
            previous,
            p,
            policy,
            endpoint_origins,
            false,
        )
    }
    pub fn maintained_closed_from_patch(
        law: &crate::patch_unilateral_contact::PatchUnilateralLaw,
        trace: &CsrMatrix,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        let data = law
            .trace_constraint_data(n, current, previous, p)?
            .ok_or_else(|| CaeError::contract("maintained contact lacks trace rows"))?;
        let origins = data
            .end
            .branches
            .iter()
            .enumerate()
            .filter_map(|(row, b)| {
                (b.gap_m == 0. && b.multiplier_n == 0.)
                    .then_some((row, ContactOriginSelection::Closed))
            })
            .collect();
        Self::from_patch_mode(law, trace, n, current, previous, p, policy, &origins, true)
    }
    fn from_patch_mode(
        law: &crate::patch_unilateral_contact::PatchUnilateralLaw,
        trace: &CsrMatrix,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        policy: ContactTangentPolicy,
        endpoint_origins: &std::collections::BTreeMap<usize, ContactOriginSelection>,
        maintained: bool,
    ) -> CaeResult<Self> {
        let mut data = law
            .trace_constraint_data(n, current, previous, p)?
            .ok_or_else(|| CaeError::contract("release law has no authentic contact trace rows"))?;
        for (&row, &selection) in endpoint_origins {
            let branch =
                data.end.branches.get(row).ok_or_else(|| {
                    CaeError::contract("release endpoint selection row out of range")
                })?;
            if branch.gap_m != 0.
                || branch.multiplier_n != 0.
                || selection != ContactOriginSelection::Closed
            {
                return Err(CaeError::contract(
                    "finite release endpoint selection must retain an actual closed origin",
                ));
            }
            data.end.origins.insert(row, selection);
        }
        if data.start.closed_rows.is_none() {
            return Err(CaeError::contract(
                "release law has no closed corner row certificate",
            ));
        }
        let (kinematics, _sites, trace_map, design_map, trace_rows) = law.release_binding();
        if trace.ncols() != law.state_size()
            || trace_rows.len() != trace_map.len()
            || trace_map.iter().any(|&r| r >= trace.nrows())
        {
            return Err(CaeError::contract(
                "release trace observation shape differs from authentic law",
            ));
        }
        for (&row, actual) in trace_map.iter().zip(trace_rows) {
            let (columns, values) = trace.row(row);
            let supplied = columns
                .iter()
                .copied()
                .zip(values.iter().copied())
                .filter(|(_, v)| *v != 0.)
                .collect::<Vec<_>>();
            if supplied != *actual {
                return Err(CaeError::contract(
                    "release trace observation differs from authentic law",
                ));
            }
        }
        let previous_chart_gaps = law.release_chart_gaps(previous, p.design)?;
        let current_chart_gaps = law.release_chart_gaps(current, p.design)?;
        if previous_chart_gaps.len() != current_chart_gaps.len() {
            return Err(CaeError::contract("release chart endpoint count"));
        }
        let mut corners = std::collections::BTreeMap::new();
        let mut open = std::collections::BTreeMap::new();
        let mut closed_endpoint_corners = Vec::new();
        for (chart, (a, b)) in previous_chart_gaps
            .iter()
            .zip(&current_chart_gaps)
            .enumerate()
        {
            if a.iter().all(|&g| g == 0.) {
                if b.iter().any(|&g| !g.is_finite() || g < 0.) {
                    return Err(CaeError::contract(
                        "release chart leaves nonnegative gap cone",
                    ));
                }
                corners.insert(chart, [true; 3]);
                let flags = b.map(|g| g > 0.);
                if flags.iter().any(|&v| v) {
                    open.insert(chart, flags);
                }
                for (corner, &gap) in b.iter().enumerate() {
                    if gap == 0. {
                        closed_endpoint_corners.push((chart, corner));
                    }
                }
            }
        }
        if corners.is_empty() {
            return Err(CaeError::contract(
                "release has no authentic closed start chart",
            ));
        }
        let start = trace
            .matvec(previous)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        let end = trace
            .matvec(current)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        if start
            .iter()
            .chain(&end)
            .chain(p.design)
            .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "release path contains a nonfinite value",
            ));
        }
        if maintained && !open.is_empty() {
            return Err(CaeError::contract(
                "maintained closed path has an actual opening",
            ));
        }
        let mut owner = Self::new_mode(trace, data, policy, maintained)?;
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"native-closed-release-v1");
        digest.update([u8::from(maintained)]);
        digest.update(n.to_le_bytes());
        digest.update(kinematics.binding_identity().as_bytes());
        for values in [previous, current, p.design] {
            digest.update(values.len().to_le_bytes());
            for value in values {
                digest.update(value.to_bits().to_le_bytes());
            }
        }
        for indices in [trace_map, design_map] {
            digest.update(indices.len().to_le_bytes());
            for index in indices {
                digest.update(index.to_le_bytes());
            }
        }
        for charts in [&corners, &open] {
            digest.update(charts.len().to_le_bytes());
            for chart in charts.keys() {
                digest.update(chart.to_le_bytes());
            }
        }
        for (&row, &selection) in endpoint_origins {
            digest.update(row.to_le_bytes());
            digest.update([match selection {
                ContactOriginSelection::Closed => 0,
                ContactOriginSelection::Open => 1,
            }]);
        }
        for value in [
            p.time_scale,
            policy.rank_relative_tolerance,
            policy.compatibility_relative_tolerance,
        ] {
            digest.update(value.to_bits().to_le_bytes());
        }
        let replay_identity = format!("native-closed-release-v1:{:x}", digest.finalize());
        let certificate = Arc::new(NativeClosedReleaseCertificate {
            replay_identity,
            extension: owner.extension.clone(),
            compatibility_relative_tolerance: policy.compatibility_relative_tolerance,
            kinematic_identity: kinematics.binding_identity(),
            trace_map: trace_map.to_vec(),
            design_map: design_map.to_vec(),
            start: start.iter().map(|v| v.to_bits()).collect(),
            end: end.iter().map(|v| v.to_bits()).collect(),
            design: p.design.iter().map(|v| v.to_bits()).collect(),
            closed_endpoint_corners,
            released_corners: open
                .iter()
                .flat_map(|(&chart, corners)| {
                    corners
                        .iter()
                        .enumerate()
                        .filter_map(move |(corner, &open)| open.then_some((chart, corner)))
                })
                .collect(),
            closed_start_charts: corners.keys().copied().collect(),
            released_charts: open.keys().copied().collect(),
            n,
            time_scale: p.time_scale.to_bits(),
        });
        owner.certificate = Some(certificate);
        Ok(owner)
    }

    pub fn certificate(&self) -> Option<&Arc<NativeClosedReleaseCertificate>> {
        self.certificate.as_ref()
    }

    pub fn released_rows(&self) -> &[usize] {
        &self.released_rows
    }

    pub fn maintained_origin_rows(&self) -> &[usize] {
        &self.maintained_origin_rows
    }
}

impl TraceDerivativeExtension for NativeClosedReleaseTrace {
    fn derivative_context_identity(&self) -> Option<String> {
        self.certificate.as_ref().map(|c| c.replay_identity.clone())
    }
    fn derivative_context(
        &self,
    ) -> Option<Arc<implexity_solve::multirate_coupling::SubcycleDerivativeContext>> {
        self.certificate.as_ref().map(|v| {
            v.clone() as Arc<implexity_solve::multirate_coupling::SubcycleDerivativeContext>
        })
    }
    fn tangent(
        &self,
        start: &[f64],
        end: &[f64],
        design: Option<&[f64]>,
        time: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.extension.tangent(start, end, design, time)
    }

    fn adjoint(&self, start: &[f64], end: &[f64]) -> CaeResult<TraceExtensionCotangent> {
        self.extension.adjoint(start, end)
    }
}

pub struct ClosedReleaseContactLaw {
    law: Arc<crate::patch_unilateral_contact::PatchUnilateralLaw>,
    endpoint_origins: std::collections::BTreeMap<usize, ContactOriginSelection>,
    initial_closed_manifold: bool,
}
impl ClosedReleaseContactLaw {
    pub fn new(law: crate::patch_unilateral_contact::PatchUnilateralLaw) -> Self {
        Self {
            law: Arc::new(law),
            endpoint_origins: std::collections::BTreeMap::new(),
            initial_closed_manifold: false,
        }
    }
    pub fn initially_closed(law: crate::patch_unilateral_contact::PatchUnilateralLaw) -> Self {
        Self {
            law: Arc::new(law),
            endpoint_origins: std::collections::BTreeMap::new(),
            initial_closed_manifold: true,
        }
    }
    pub fn with_closed_endpoint_origins(mut self, rows: &[usize]) -> CaeResult<Self> {
        for &row in rows {
            if row >= self.law.multipliers()
                || self
                    .endpoint_origins
                    .insert(row, ContactOriginSelection::Closed)
                    .is_some()
            {
                return Err(CaeError::contract(
                    "release endpoint origin row duplicate or out of range",
                ));
            }
        }
        Ok(self)
    }
    pub fn law(&self) -> &crate::patch_unilateral_contact::PatchUnilateralLaw {
        &self.law
    }
    pub fn shared_law(&self) -> Arc<crate::patch_unilateral_contact::PatchUnilateralLaw> {
        self.law.clone()
    }
    pub fn into_shared_law(self) -> Arc<crate::patch_unilateral_contact::PatchUnilateralLaw> {
        self.law
    }
}
impl crate::moving_contact::contact_field::NativeContactLaw for ClosedReleaseContactLaw {
    fn owned_numerical_trace_extension(
        &self,
        trace: &CsrMatrix,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        direction: Option<&[f64]>,
        policy: ContactTangentPolicy,
    ) -> CaeResult<Option<Arc<dyn TraceDerivativeExtension>>> {
        let data = self
            .law
            .trace_constraint_data(n, current, previous, p)?
            .ok_or_else(|| CaeError::contract("numerical contact trial lacks authentic rows"))?;
        if let Some(d) = direction {
            if d.len() != current.len() || d.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract("numerical contact trial direction"));
            }
            let selected = self
                .law
                .trace_constraint_data_for_direction(n, current, previous, p, d)?
                .ok_or_else(|| {
                    CaeError::contract("numerical trial lacks native direction selection")
                })?;
            if selected
                .end
                .origins
                .values()
                .any(|v| *v == ContactOriginSelection::Open)
            {
                return Err(CaeError::contract(
                    "numerical opening origin gas derivative is not qualified",
                ));
            }
        }
        let mut selected = [Vec::new(), Vec::new()];
        for (end, endpoint) in [(false, &data.start), (true, &data.end)] {
            for (row, branch) in endpoint.branches.iter().enumerate() {
                if !branch.gap_m.is_finite()
                    || !branch.multiplier_n.is_finite()
                    || branch.gap_m < 0.
                    || branch.multiplier_n < 0.
                {
                    return Err(CaeError::contract(
                        "numerical contact trial nonnegative domain",
                    ));
                }
                if branch.gap_m != 0. {
                    continue;
                }
                if end
                    && branch.multiplier_n == 0.
                    && !self.initial_closed_manifold
                    && self.endpoint_origins.get(&row) != Some(&ContactOriginSelection::Closed)
                {
                    return Err(CaeError::contract(
                        "numerical origin needs declared closed selection",
                    ));
                }
                if end && branch.multiplier_n == 0. {
                    if let Some(d) = direction {
                        let matrix = endpoint.contribution.constraint_current.to_csr()?;
                        let (columns, values) = matrix.row(row);
                        let terms: Vec<_> = columns
                            .iter()
                            .zip(values)
                            .map(|(&i, &v)| v * d[i])
                            .collect();
                        let dg: f64 = terms.iter().sum();
                        let error =
                            128. * f64::EPSILON * terms.iter().map(|v| v.abs()).sum::<f64>();
                        if dg.abs() > error {
                            return Err(CaeError::contract(
                                "numerical opening origin gas derivative is not qualified",
                            ));
                        }
                    }
                }
                selected[usize::from(end)].push(row);
            }
        }
        let a = trace
            .matvec(previous)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        let b = trace
            .matvec(current)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        let witness = Arc::new(NativeClosedGeometryWitness::from_patch_trace(
            &self.law, n, &a, &b, p,
        )?);
        let extension = Arc::new(ContactTraceExtension::from_selected_constraints(
            trace,
            data,
            &selected[0],
            &selected[1],
            policy,
        )?);
        let certificate = Arc::new(NativeClosedTrialCertificate {
            witness,
            extension: extension.clone(),
        });
        Ok(Some(Arc::new(NativeClosedTrialTrace {
            extension,
            certificate,
        })))
    }
    fn owned_subcycle_context(
        &self,
        n: usize,
        start: &[f64],
        end: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<Option<Arc<implexity_solve::multirate_coupling::SubcyclePrimalContext>>> {
        Ok(Some(Arc::new(
            NativeClosedGeometryWitness::from_patch_trace(&self.law, n, start, end, p)?,
        )))
    }
    fn owned_trace_derivative_extension(
        &self,
        trace: &CsrMatrix,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        policy: ContactTangentPolicy,
    ) -> CaeResult<Option<Arc<dyn TraceDerivativeExtension>>> {
        let data = self
            .law
            .trace_constraint_data(n, current, previous, p)?
            .ok_or_else(|| CaeError::contract("release contact law lacks trace constraints"))?;
        if !data
            .start
            .branches
            .iter()
            .zip(&data.end.branches)
            .any(|(a, b)| a.gap_m == 0. && b.gap_m > 0. && b.multiplier_n == 0.)
        {
            if self.initial_closed_manifold && data.start.branches.iter().any(|b| b.gap_m == 0.) {
                return Ok(Some(Arc::new(
                    NativeClosedReleaseTrace::maintained_closed_from_patch(
                        &self.law, trace, n, current, previous, p, policy,
                    )?,
                )));
            }
            return Ok(None);
        }
        let mut origins = self.endpoint_origins.clone();
        if self.initial_closed_manifold {
            for (row, b) in data.end.branches.iter().enumerate() {
                if b.gap_m == 0. && b.multiplier_n == 0. {
                    origins.insert(row, ContactOriginSelection::Closed);
                }
            }
        }
        Ok(Some(Arc::new(
            NativeClosedReleaseTrace::from_patch_with_endpoint_origins(
                &self.law, trace, n, current, previous, p, policy, &origins,
            )?,
        )))
    }
    fn check_trace_tangent_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        dc: &[f64],
        dp: &[f64],
        dd: Option<&[f64]>,
        dt: f64,
    ) -> CaeResult<()> {
        if dc.len() != current.len()
            || dp.len() != previous.len()
            || dc.iter().chain(dp).any(|v| !v.is_finite())
            || !dt.is_finite()
        {
            return Err(CaeError::contract(
                "release physical tangent shape or value",
            ));
        }
        let zero = vec![0.; p.design.len()];
        let dd = dd.unwrap_or(&zero);
        if dd.len() != p.design.len() || dd.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("release physical design tangent"));
        }
        let data = self
            .law
            .trace_constraint_data(n, current, previous, p)?
            .ok_or_else(|| CaeError::contract("release physical tangent lacks contact rows"))?;
        let states = self.law.state_size() - self.law.multipliers();
        for (endpoint, dx, dy, start) in [
            (&data.start, dp, None, true),
            (&data.end, dc, Some(dp), false),
        ] {
            let cc = endpoint.contribution.constraint_current.to_csr()?;
            let cp = endpoint.contribution.constraint_previous.to_csr()?;
            let cd = endpoint.contribution.constraint_design.to_csr()?;
            for (row, branch) in endpoint.branches.iter().enumerate() {
                if branch.gap_m != 0. || branch.multiplier_n != 0. {
                    continue;
                }
                let selection = if start || self.initial_closed_manifold {
                    Some(ContactOriginSelection::Closed)
                } else {
                    self.endpoint_origins.get(&row).copied()
                };
                let mut terms = Vec::new();
                for (matrix, values) in [(&cc, Some(dx)), (&cp, dy), (&cd, Some(dd))] {
                    if let Some(values) = values {
                        let (columns, weights) = matrix.row(row);
                        for (&j, &w) in columns.iter().zip(weights) {
                            terms.push(w * values[j]);
                        }
                    }
                }
                terms.push(endpoint.contribution.constraint_time[row] * dt);
                let dg: f64 = terms.iter().sum();
                let gap_roundoff = 128. * f64::EPSILON * terms.iter().map(|v| v.abs()).sum::<f64>();
                let dl = dx[states + row];
                let selections = selection.into_iter().map(|v| (0, v)).collect();
                crate::contact_constraint_tangent::validate_contact_origin_direction(
                    &[*branch],
                    &selections,
                    &[dg],
                    &[dl],
                    gap_roundoff,
                    128. * f64::EPSILON * dl.abs(),
                )?;
            }
        }
        Ok(())
    }
    fn newton_direction_constraints(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        direction: &[f64],
    ) -> CaeResult<Vec<(usize, f64)>> {
        self.law
            .newton_direction_constraints(n, current, previous, p, direction)
    }
    fn trace_constraint_data(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<Option<ContactTraceConstraintPair>> {
        self.law.trace_constraint_data(n, current, previous, p)
    }
    fn trace_constraint_data_for_direction(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        direction: &[f64],
    ) -> CaeResult<Option<ContactTraceConstraintPair>> {
        self.law
            .trace_constraint_data_for_direction(n, current, previous, p, direction)
    }
    fn check_trace_derivative_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<()> {
        self.law.check_state_domain(n, current, previous, p)?;
        let data = self
            .law
            .trace_constraint_data(n, current, previous, p)?
            .ok_or_else(|| CaeError::contract("release contact law lacks trace constraints"))?;
        for (row, branch) in data.end.branches.iter().enumerate() {
            if branch.gap_m == 0.
                && branch.multiplier_n == 0.
                && !self.initial_closed_manifold
                && self.endpoint_origins.get(&row) != Some(&ContactOriginSelection::Closed)
            {
                return Err(CaeError::contract(
                    "release endpoint origin has no selected closed trace derivative",
                ));
            }
        }
        for &row in self.endpoint_origins.keys() {
            let branch = data
                .end
                .branches
                .get(row)
                .ok_or_else(|| CaeError::contract("release endpoint row out of range"))?;
            if branch.gap_m != 0. || branch.multiplier_n != 0. {
                return Err(CaeError::contract(
                    "release endpoint selection differs from actual origin",
                ));
            }
        }
        Ok(())
    }
    fn step_ledger(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<std::collections::BTreeMap<String, f64>> {
        self.law.step_ledger(n, current, previous, p)
    }
    fn check_derivative_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<()> {
        self.law.check_derivative_domain(n, current, previous, p)
    }
    fn check_state_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<()> {
        self.law.check_state_domain(n, current, previous, p)
    }
    fn newton_constraints(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        direction: &[f64],
        attempt: usize,
    ) -> CaeResult<Option<implexity_solve::matrix::Jacobian>> {
        self.law
            .newton_constraints(n, current, previous, p, direction, attempt)
    }
    fn evaluate(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
    ) -> CaeResult<crate::moving_contact::contact_field::ContactContribution> {
        self.law.evaluate(n, current, previous, p)
    }
    fn multipliers(&self) -> usize {
        self.law.multipliers()
    }
    fn admitted_trial(
        &self,
        n: usize,
        current: &[f64],
        direction: &[f64],
        previous: &[f64],
        p: implexity_solve::time_stepper::StepParameters<'_>,
        trial: f64,
    ) -> CaeResult<f64> {
        self.law
            .admitted_trial(n, current, direction, previous, p, trial)
    }
}
