// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
    fn second_order(&self) -> Option<&dyn SecondOrderSubcycle> { T::second_order(*self) }
}

impl<T: FluxDrivenField + ?Sized> FluxDrivenField for &T {
    fn local_elimination_groups(&self) -> CaeResult<Option<Vec<Vec<usize>>>> { T::local_elimination_groups(*self) }
    fn check_derivative_domain(&self, _n: usize, _current: &[f64], _previous: &[f64], _p: StepParameters<'_>) -> CaeResult<()> { T::check_derivative_domain(*self ,_n,_current,_previous,_p) }
    fn check_state_domain(&self, _n: usize, _current: &[f64], _previous: &[f64], _p: StepParameters<'_>) -> CaeResult<()> { T::check_state_domain(*self ,_n,_current,_previous,_p) }
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
