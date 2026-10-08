// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::field::AnyMovingLbm;
use implexity_solve::multirate_coupling::{
    SecondOrderSubcycle, SubcycleCotangent, SubcycleRecord, SubcycleTangent, SubcycledField,
};
use implexity_solve::time_stepper::StepParameters;

use crate::carrier::SolidCarrier;

const FLUX_CACHE: usize = 8;

pub struct AbsoluteSubcycleCotangent{pub ordinary:SubcycleCotangent,pub absolute_origin_s:f64}

pub struct FluidField {
    inner: AnyMovingLbm,
    power: Vec<String>,
    names: Vec<String>,
    nominal_step_s: f64,
    cache: Mutex<Vec<([u8; 32], Vec<f64>)>>,
    void_ledger: Option<Arc<SolidCarrier>>,
}

pub const LEDGER_SKIPPED_VOID: &str = "pushforward_skipped_void_elements";

impl std::fmt::Debug for FluidField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FluidField")
            .field("inner", &self.inner)
            .field("power", &self.power)
            .finish_non_exhaustive()
    }
}

fn digest(n: usize, p: StepParameters<'_>, parts: &[&[f64]]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update((n as u64).to_le_bytes());
    h.update(p.time_scale.to_bits().to_le_bytes());
    for part in std::iter::once(&p.design).chain(parts) {
        h.update((part.len() as u64).to_le_bytes());
        for v in *part {
            h.update(v.to_bits().to_le_bytes());
        }
    }
    h.finalize().into()
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

impl FluidField {
    pub fn with_absolute_interval_origin(mut self,clock:implexity_physics_lbm::moving::field::AbsoluteIntervalOrigin)->Self{self.inner=self.inner.with_absolute_interval_origin(clock);if let Ok(mut c)=self.cache.lock(){c.clear();}self}
    pub fn event_tick(&self,previous:&[f64],design:&[f64],trace:implexity_physics_lbm::moving::field::event_trace::EventTickTrace<'_>)->CaeResult<implexity_physics_lbm::moving::field::event_trace::EventTickOutput>{
        let mut out=self.inner.event_tick(previous,design,trace)?;
        if self.has_power(){let dt=self.inner.nominal_fluid_step_s();let mut work=0.;for i in 0..trace.start.len(){let impulse=out.before_impulse_n_s[i]+out.after_impulse_n_s[i]+out.start_compensation_impulse_n_s[i]+out.end_compensation_impulse_n_s[i];work-=impulse*(trace.end[i]-trace.start[i])/(dt*dt);}if !work.is_finite(){return Err(CaeError::contract("event joint interface power overflow"));}out.samples.extend(std::iter::repeat_n(work,self.power.len()));}Ok(out)
    }
    pub fn event_tick_tangent(&self,previous:&[f64],design:&[f64],trace:implexity_physics_lbm::moving::field::event_trace::EventTickTrace<'_>,direction:implexity_physics_lbm::moving::field::event_trace::EventTickDirection<'_>)->CaeResult<implexity_physics_lbm::moving::field::event_trace::EventTickTangent>{
        let mut out=self.inner.event_tick_tangent(previous,design,trace,direction)?;
        if self.has_power(){let dt=self.inner.nominal_fluid_step_s();let(mut work,mut dwork)=(0.,0.);for i in 0..trace.start.len(){let impulse=out.primal.before_impulse_n_s[i]+out.primal.after_impulse_n_s[i]+out.primal.start_compensation_impulse_n_s[i]+out.primal.end_compensation_impulse_n_s[i];let d=out.before_impulse_n_s[i]+out.after_impulse_n_s[i]+out.start_compensation_impulse_n_s[i]+out.end_compensation_impulse_n_s[i];let u=trace.end[i]-trace.start[i];let du=direction.end[i]-direction.start[i];work-=impulse*u/(dt*dt);dwork-=(d*u+impulse*du)/(dt*dt);}if !work.is_finite()||!dwork.is_finite(){return Err(CaeError::contract("event joint interface power direction overflow"));}out.primal.samples.extend(std::iter::repeat_n(work,self.power.len()));out.samples.extend(std::iter::repeat_n(dwork,self.power.len()));}Ok(out)
    }
    pub fn event_tick_adjoint(&self,previous:&[f64],design:&[f64],trace:implexity_physics_lbm::moving::field::event_trace::EventTickTrace<'_>,bar:implexity_physics_lbm::moving::field::event_trace::EventTickCotangent<'_>)->CaeResult<implexity_physics_lbm::moving::field::event_trace::EventTickBars>{
        if !self.has_power(){return self.inner.event_tick_adjoint(previous,design,trace,bar);}
        let na=self.inner.sample_names().len();if bar.samples.len()!=na+self.power.len()||bar.samples.iter().any(|x|!x.is_finite()){return Err(CaeError::contract("event joint interface power cotangent shape"));}
        let pb:f64=bar.samples[na..].iter().sum();let dt=self.inner.nominal_fluid_step_s();let out=self.inner.event_tick(previous,design,trace)?;
        let adjust=|v:&[f64]|->CaeResult<Vec<f64>>{if v.len()!=trace.start.len(){return Err(CaeError::contract("event power impulse cotangent shape"));}Ok(v.iter().enumerate().map(|(i,v)|v-pb*(trace.end[i]-trace.start[i])/(dt*dt)).collect())};
        let b0=adjust(bar.before_impulse_n_s)?;let b1=adjust(bar.after_impulse_n_s)?;let c0=adjust(bar.start_compensation_impulse_n_s)?;let c1=adjust(bar.end_compensation_impulse_n_s)?;
        let mut z=self.inner.event_tick_adjoint(previous,design,trace,implexity_physics_lbm::moving::field::event_trace::EventTickCotangent{state:bar.state,before_impulse_n_s:&b0,after_impulse_n_s:&b1,start_compensation_impulse_n_s:&c0,end_compensation_impulse_n_s:&c1,samples:&bar.samples[..na]})?;
        for i in 0..trace.start.len(){let impulse=out.before_impulse_n_s[i]+out.after_impulse_n_s[i]+out.start_compensation_impulse_n_s[i]+out.end_compensation_impulse_n_s[i];z.start[i]+=pb*impulse/(dt*dt);z.end[i]-=pb*impulse/(dt*dt);}
        if z.start.iter().chain(&z.end).any(|x|!x.is_finite()){return Err(CaeError::contract("event power trace cotangent overflow"));}Ok(z)
    }
    pub fn subcycle_adjoint_with_origin(&self,n:usize,previous:&[f64],start:&[f64],end:&[f64],p:StepParameters<'_>,state_bar:&[f64],flux_bar:&[f64],sample_bar:&[f64])->CaeResult<AbsoluteSubcycleCotangent>{
        let ordinary=SubcycledField::subcycle_adjoint(self,n,previous,start,end,p,state_bar,flux_bar,sample_bar)?;
        let mut native_flux_bar=flux_bar.to_vec();
        if self.has_power(){let dt=self.step(p);if sample_bar.len()!=self.names.len(){return Err(CaeError::contract("absolute interface sample cotangent size"));}let power_bar:f64=sample_bar[self.inner.sample_names().len()..].iter().sum();for i in 0..native_flux_bar.len(){native_flux_bar[i]-=power_bar*(end[i]-start[i])/dt;}}
        let bars=self.inner.subcycle_adjoint_with_origin(n,previous,start,end,p.design,p.time_scale,state_bar,&native_flux_bar,&sample_bar[..self.inner.sample_names().len()])?;
        Ok(AbsoluteSubcycleCotangent{ordinary,absolute_origin_s:bars.absolute_origin_s})
    }
    #[must_use]
    pub fn new(inner: AnyMovingLbm, power: Vec<String>, nominal_step_s: f64) -> Self {
        let mut names = inner.sample_names().to_vec();
        names.extend(power.iter().cloned());
        Self { inner, power, names, nominal_step_s, cache: Mutex::new(Vec::new()), void_ledger: None }
    }

    #[must_use]
    pub fn with_void_ledger(mut self, carrier: Arc<SolidCarrier>) -> Self {
        self.void_ledger = Some(carrier);
        self
    }

    #[must_use]
    pub fn inner(&self) -> &AnyMovingLbm {
        &self.inner
    }

    fn has_power(&self) -> bool {
        !self.power.is_empty()
    }

    fn step(&self, p: StepParameters<'_>) -> f64 {
        p.time_scale * self.nominal_step_s
    }

    fn remember(&self, key: [u8; 32], flux: &[f64]) {
        if let Ok(mut c) = self.cache.lock() {
            if c.iter().any(|(k, _)| *k == key) {
                return;
            }
            c.push((key, flux.to_vec()));
            if c.len() > FLUX_CACHE {
                c.remove(0);
            }
        }
    }

    fn flux(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> {
        let key = digest(n, p, &[previous, trace_start, trace_end]);
        if let Ok(c) = self.cache.lock()
            && let Some((_, f)) = c.iter().find(|(k, _)| *k == key)
        {
            return Ok(f.clone());
        }
        let rec = self.inner.subcycle(n, previous, trace_start, trace_end, p)?;
        self.remember(key, &rec.flux);
        Ok(rec.flux)
    }

    fn check_traces(trace_start: &[f64], trace_end: &[f64]) -> CaeResult<()> {
        if trace_start.len() == trace_end.len() {
            Ok(())
        } else {
            Err(CaeError::contract("interface power: trace lengths differ"))
        }
    }
}

impl SubcycledField for FluidField {
    fn state_size(&self) -> usize {
        self.inner.state_size()
    }

    fn trace_size(&self) -> usize {
        self.inner.trace_size()
    }

    fn design_size(&self) -> usize {
        self.inner.design_size()
    }

    fn sample_names(&self) -> &[String] {
        &self.names
    }

    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        self.inner.initial_state(design)
    }

    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        self.inner.initial_state_vjp(design, cotangent)
    }

    fn subcycle(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> {
        let mut rec = self.inner.subcycle(n, previous, trace_start, trace_end, p)?;
        if let Some(carrier) = &self.void_ledger {
            #[allow(clippy::cast_precision_loss)]
            let skipped = carrier.skipped_void_elements(trace_end, p.design)? as f64;
            rec.ledger.insert(LEDGER_SKIPPED_VOID.to_string(), skipped);
        }
        if self.has_power() {
            Self::check_traces(trace_start, trace_end)?;
            self.remember(digest(n, p, &[previous, trace_start, trace_end]), &rec.flux);
            let dd: Vec<f64> = trace_end.iter().zip(trace_start).map(|(e, s)| e - s).collect();
            let power = -dot(&rec.flux, &dd) / self.step(p);
            rec.samples.extend(std::iter::repeat_n(power, self.power.len()));
        }
        Ok(rec)
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
        let mut t = self.inner.subcycle_tangent(
            n,
            previous,
            trace_start,
            trace_end,
            p,
            d_previous,
            d_trace_start,
            d_trace_end,
            d_design,
            d_time_scale,
        )?;
        if self.has_power() {
            Self::check_traces(trace_start, trace_end)?;
            let f = self.flux(n, previous, trace_start, trace_end, p)?;
            let dt = self.step(p);
            let dd: Vec<f64> = trace_end.iter().zip(trace_start).map(|(e, s)| e - s).collect();
            let ddd: Vec<f64> = d_trace_end.iter().zip(d_trace_start).map(|(e, s)| e - s).collect();
            let power = -dot(&f, &dd) / dt;
            let dp = -(dot(&t.flux, &dd) + dot(&f, &ddd)) / dt - power * d_time_scale / p.time_scale;
            t.samples.extend(std::iter::repeat_n(dp, self.power.len()));
        }
        Ok(t)
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
        let na = self.inner.sample_names().len();
        if !self.has_power() || sample_bar.is_empty() {
            return self.inner.subcycle_adjoint(
                n,
                previous,
                trace_start,
                trace_end,
                p,
                state_bar,
                flux_bar,
                sample_bar,
            );
        }
        if sample_bar.len() != na + self.power.len() {
            return Err(CaeError::contract("interface power: sample cotangent has the wrong length"));
        }
        let pbar: f64 = sample_bar[na..].iter().sum();
        let inner_bar = &sample_bar[..na];
        if pbar == 0.0 {
            return self.inner.subcycle_adjoint(
                n,
                previous,
                trace_start,
                trace_end,
                p,
                state_bar,
                flux_bar,
                inner_bar,
            );
        }
        Self::check_traces(trace_start, trace_end)?;
        let f = self.flux(n, previous, trace_start, trace_end, p)?;
        let dt = self.step(p);
        let dd: Vec<f64> = trace_end.iter().zip(trace_start).map(|(e, s)| e - s).collect();
        let power = -dot(&f, &dd) / dt;
        let fb: Vec<f64> = if flux_bar.is_empty() {
            dd.iter().map(|d| -pbar * d / dt).collect()
        } else {
            flux_bar.iter().zip(&dd).map(|(b, d)| b - pbar * d / dt).collect()
        };
        let mut cot =
            self.inner.subcycle_adjoint(n, previous, trace_start, trace_end, p, state_bar, &fb, inner_bar)?;
        for ((e, s), fv) in cot.trace_end.iter_mut().zip(cot.trace_start.iter_mut()).zip(&f) {
            *e -= pbar * fv / dt;
            *s += pbar * fv / dt;
        }
        cot.time_scale -= pbar * power / p.time_scale;
        Ok(cot)
    }

    fn second_order(&self) -> Option<&dyn SecondOrderSubcycle> {

        if self.has_power() { None } else { self.inner.second_order() }
    }
}
