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

use super::*;

impl<T: SubcycledField + ?Sized> SubcycledField for &T {
    fn state_size(&self) -> usize { T::state_size(*self) }
    fn trace_size(&self) -> usize { T::trace_size(*self) }
    fn design_size(&self) -> usize { T::design_size(*self) }
    fn sample_names(&self) -> &[String] { T::sample_names(*self) }
    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> { T::initial_state(*self ,design) }
    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> { T::initial_state_vjp(*self ,design,cotangent) }
    fn subcycle(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> { T::subcycle(*self ,n,previous,trace_start,trace_end,p) }
    fn subcycle_with_context(&self,n:usize,previous:&[f64],start:&[f64],end:&[f64],p:StepParameters<'_>,context:Option<&SubcyclePrimalContext>)->CaeResult<SubcycleRecord> {
        T::subcycle_with_context(*self,n,previous,start,end,p,context)
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
    ) -> CaeResult<SubcycleTangent> { T::subcycle_tangent(*self ,n,previous,trace_start,trace_end,p,d_previous,d_trace_start,d_trace_end,d_design,d_time_scale) }
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
    ) -> CaeResult<SubcycleCotangent> { T::subcycle_adjoint(*self ,n,previous,trace_start,trace_end,p,state_bar,flux_bar,sample_bar) }
    fn subcycle_tangent_with_context(&self,n:usize,previous:&[f64],start:&[f64],end:&[f64],p:StepParameters<'_>,dp:&[f64],ds:&[f64],de:&[f64],dd:Option<&[f64]>,dt:f64,context:Option<&SubcycleDerivativeContext>)->CaeResult<SubcycleTangent> {
        T::subcycle_tangent_with_context(*self,n,previous,start,end,p,dp,ds,de,dd,dt,context)
    }
    fn subcycle_adjoint_with_context(&self,n:usize,previous:&[f64],start:&[f64],end:&[f64],p:StepParameters<'_>,state_bar:&[f64],flux_bar:&[f64],sample_bar:&[f64],context:Option<&SubcycleDerivativeContext>)->CaeResult<SubcycleCotangent> {
        T::subcycle_adjoint_with_context(*self,n,previous,start,end,p,state_bar,flux_bar,sample_bar,context)
    }
    fn second_order(&self) -> Option<&dyn SecondOrderSubcycle> { T::second_order(*self) }
}

impl<T: FluxDrivenField + ?Sized> FluxDrivenField for &T {
    fn subcycle_context(&self,n:usize,start:&[f64],end:&[f64],p:StepParameters<'_>)->CaeResult<Option<std::sync::Arc<SubcyclePrimalContext>>> {
        T::subcycle_context(*self,n,start,end,p)
    }
    fn check_tangent_domain(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>,dc:&[f64],dp:&[f64],dd:Option<&[f64]>,dt:f64)->CaeResult<()> {
        T::check_tangent_domain(*self,n,current,previous,p,dc,dp,dd,dt)
    }
    fn trace_derivative_extension(&self, n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<Option<std::sync::Arc<dyn TraceDerivativeExtension>>> {
        T::trace_derivative_extension(*self, n, current, previous, p)
    }
    fn numerical_trace_derivative_extension(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<Option<std::sync::Arc<dyn TraceDerivativeExtension>>> { T::numerical_trace_derivative_extension(*self,n,current,previous,p) }
    fn numerical_trace_derivative_extension_for_direction(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>,direction:&[f64])->CaeResult<Option<std::sync::Arc<dyn TraceDerivativeExtension>>> { T::numerical_trace_derivative_extension_for_direction(*self,n,current,previous,p,direction) }
    fn trace_derivative_extension_for_direction(&self, n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>, direction: &[f64]) -> CaeResult<Option<std::sync::Arc<dyn TraceDerivativeExtension>>> {
        T::trace_derivative_extension_for_direction(*self, n, current, previous, p, direction)
    }

    fn local_elimination_groups(&self) -> CaeResult<Option<Vec<Vec<usize>>>> { T::local_elimination_groups(*self) }
    fn check_derivative_domain(&self, _n: usize, _current: &[f64], _previous: &[f64], _p: StepParameters<'_>) -> CaeResult<()> { T::check_derivative_domain(*self ,_n,_current,_previous,_p) }
    fn check_state_domain(&self, _n: usize, _current: &[f64], _previous: &[f64], _p: StepParameters<'_>) -> CaeResult<()> { T::check_state_domain(*self ,_n,_current,_previous,_p) }
    fn newton_direction_constraints(&self,n:usize,current:&[f64],previous:&[f64],flux:&[f64],p:StepParameters<'_>,direction:&[f64])->CaeResult<Vec<(usize,f64)>> {
        (**self).newton_direction_constraints(n,current,previous,flux,p,direction)
    }
    fn newton_alternative(&self, _n: usize, _current: &[f64], _previous: &[f64], _flux: &[f64], _p: StepParameters<'_>, _direction: &[f64], _attempt: usize) -> CaeResult<Option<Jacobian>> { T::newton_alternative(*self ,_n,_current,_previous,_flux,_p,_direction,_attempt) }
    fn state_size(&self) -> usize { T::state_size(*self) }
    fn design_size(&self) -> usize { T::design_size(*self) }
    fn sample_names(&self) -> &[String] { T::sample_names(*self) }
    fn nominal_step_s(&self) -> f64 { T::nominal_step_s(*self) }
    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> { T::initial_state(*self ,design) }
    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> { T::initial_state_vjp(*self ,design,cotangent) }
    fn trace_operator(&self) -> &CsrMatrix { T::trace_operator(*self) }
    fn check_trace_path(&self, n: usize, start: &[f64], end: &[f64], p: StepParameters<'_>) -> CaeResult<()> { T::check_trace_path(*self ,n,start,end,p) }
    fn predict(
        &self,
        n: usize,
        previous: &[f64],
        before_previous: Option<&[f64]>,
        p: StepParameters<'_>,
    ) -> Vec<f64> { T::predict(*self ,n,previous,before_previous,p) }
    fn residual(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> { T::residual(*self ,n,current,previous,flux,p) }
    fn jacobians(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<FieldJacobians> { T::jacobians(*self ,n,current,previous,flux,p) }
    fn admissible_step(&self, n: usize, current: &[f64], direction: &[f64], previous: &[f64], p: StepParameters<'_>, trial: f64) -> CaeResult<f64> { T::admissible_step(*self ,n,current,direction,previous,p,trial) }
    fn current_jacobian(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Jacobian> { T::current_jacobian(*self ,n,current,previous,flux,p) }
    fn current_flux_jacobians(&self, n: usize, current: &[f64], previous: &[f64], flux: &[f64], p: StepParameters<'_>) -> CaeResult<(Jacobian, Jacobian)> { T::current_flux_jacobians(*self ,n,current,previous,flux,p) }
    fn samples(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> { T::samples(*self ,n,current,previous,p) }
    fn samples_vjp(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        sample_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>)> { T::samples_vjp(*self ,n,current,previous,p,sample_bar) }
    fn explicit(&self) -> bool { T::explicit(*self) }
    fn second_order(&self) -> Option<&dyn SecondOrderField> { T::second_order(*self) }
}
