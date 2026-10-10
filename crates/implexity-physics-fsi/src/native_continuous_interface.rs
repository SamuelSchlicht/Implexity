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

use crate::native_continuous_coupled_field::NativeContinuousCoupledField;
use implexity_core::{CaeError, CaeResult};
use implexity_solve::{
    multirate_coupling::{SubcycleCotangent, SubcycleRecord, SubcycleTangent, SubcycledField},
    time_stepper::StepParameters,
};

impl SubcycledField for NativeContinuousCoupledField {
    fn state_size(&self) -> usize {
        NativeContinuousCoupledField::state_size(self)
    }
    fn trace_size(&self) -> usize {
        self.config.bulk.trace_size()
    }
    fn design_size(&self) -> usize {
        self.config.bulk.design_size()
    }
    fn sample_names(&self) -> &[String] {
        self.config.bulk.sample_names()
    }
    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        NativeContinuousCoupledField::initial_state(self, design)
    }
    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        self.initial_state_pullback(design, cotangent)
    }
    fn subcycle(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> {
        self.forward(n, previous, trace_start, trace_end, p)
    }
    fn subcycle_with_context(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        context: Option<&implexity_solve::multirate_coupling::SubcyclePrimalContext>,
    ) -> CaeResult<SubcycleRecord> {
        let owned=match context {
            None=>None,
            Some(context)=>Some(crate::native_continuous_coupled_field::NativeContinuousGeometryContext::Primal(std::sync::Arc::new(context.downcast_ref::<crate::native_closed_release::NativeClosedGeometryWitness>().ok_or_else(||CaeError::contract("native field does not own this primal geometry context"))?.clone()))),
        };
        self.forward_with_closed_context(n, previous, start, end, p, owned.as_ref())
    }
    fn subcycle_tangent_with_context(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        d_previous: &[f64],
        d_start: &[f64],
        d_end: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
        context: Option<&implexity_solve::multirate_coupling::SubcycleDerivativeContext>,
    ) -> CaeResult<SubcycleTangent> {
        let certificate = geometry_context(context)?;
        let zero = vec![0.; p.design.len()];
        self.tangent_with_closed_context(
            n,
            previous,
            start,
            end,
            p,
            d_previous,
            d_start,
            d_end,
            d_design.unwrap_or(&zero),
            d_time_scale,
            certificate.as_ref(),
        )
    }
    fn subcycle_tangent(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        d_previous: &[f64],
        d_trace_start: &[f64],
        d_trace_end: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<SubcycleTangent> {
        let zero;
        let design_direction = match d_design {
            Some(direction) => direction,
            None => {
                zero = vec![0.; p.design.len()];
                &zero
            }
        };
        self.tangent(
            n,
            previous,
            trace_start,
            trace_end,
            p,
            d_previous,
            d_trace_start,
            d_trace_end,
            design_direction,
            d_time_scale,
        )
    }
    fn subcycle_adjoint_with_context(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
        context: Option<&implexity_solve::multirate_coupling::SubcycleDerivativeContext>,
    ) -> CaeResult<SubcycleCotangent> {
        let certificate = geometry_context(context)?;
        self.adjoint_with_closed_context(
            n,
            previous,
            start,
            end,
            p,
            state_bar,
            flux_bar,
            sample_bar,
            certificate.as_ref(),
        )
    }
    fn subcycle_adjoint(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleCotangent> {
        self.adjoint(
            n,
            previous,
            trace_start,
            trace_end,
            p,
            state_bar,
            flux_bar,
            sample_bar,
        )
    }
}

fn geometry_context(
    context: Option<&implexity_solve::multirate_coupling::SubcycleDerivativeContext>,
) -> CaeResult<Option<crate::native_continuous_coupled_field::NativeContinuousGeometryContext>> {
    use crate::native_continuous_coupled_field::NativeContinuousGeometryContext;
    match context {
        None => Ok(None),
        Some(context) => {
            if let Some(c) = context
                .downcast_ref::<crate::native_closed_release::NativeClosedReleaseCertificate>()
            {
                Ok(Some(NativeContinuousGeometryContext::Derivative(
                    std::sync::Arc::new(c.clone()),
                )))
            } else if let Some(c) =
                context.downcast_ref::<crate::native_closed_release::NativeClosedTrialCertificate>()
            {
                Ok(Some(NativeContinuousGeometryContext::Trial(
                    std::sync::Arc::new(c.clone()),
                )))
            } else {
                Err(CaeError::contract(
                    "native field does not own this derivative geometry context",
                ))
            }
        }
    }
}
