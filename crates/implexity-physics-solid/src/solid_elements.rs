// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use serde_json::Value;

use implexity_ad::Scalar;
use implexity_solve::local_assembly::LocalResidual;

use crate::history::MaterialHistoryBinding;
use crate::inelastic::{CreepLaw, InelasticLayout, PlasticLaw};
use crate::mandel::{self, IDENTITY, Mandel, SQRT2};
use crate::material::{MaterialLaw, Props, SolidMaterial, idx};
use crate::polymer::{BoundMaxwell, MaxwellResponse};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Exponent {
    Int(i32),
    Float(f64),
}

impl Exponent {
    #[must_use]
    pub fn from_json(v: &Value) -> Self {
        match v.as_i64() {
            Some(i) if v.is_i64() || v.is_u64() => Self::Int(i32::try_from(i).unwrap_or(i32::MAX)),
            _ => Self::Float(v.as_f64().unwrap_or(f64::NAN)),
        }
    }

    pub fn apply<S: Scalar>(self, x: S) -> S {
        match self {
            Self::Int(n) => x.powi(n),
            Self::Float(y) => x.powf(y),
        }
    }

    #[must_use]
    pub fn value(self) -> f64 {
        match self {
            Self::Int(n) => f64::from(n),
            Self::Float(y) => y,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SolidModel {
    pub t0: f64,
    pub ts: f64,
    pub es: f64,
    pub ss: f64,
    pub ls: f64,
    pub ks: f64,
    pub us: f64,
    pub stiffness_floor: f64,
    pub conductivity_floor: f64,
    pub penalty: Exponent,
    pub materials: [SolidMaterial; 2],
    pub law: MaterialLaw,
    pub numerical: bool,
    pub plastic: Option<PlasticLaw>,
    pub creep: Option<CreepLaw>,
    pub viscoelastic: Option<BoundMaxwell>,
    pub history: Option<MaterialHistoryBinding>,
    pub layout: InelasticLayout,
    pub scales: Vec<f64>,
    pub internal_size: usize,
    pub forcing_channels: usize,
}

#[derive(Debug, Clone)]
pub struct ElementFields<S> {
    pub temperature: [S; 4],
    pub state: Vec<S>,
    pub grad: [[S; 3]; 4],
    pub volume: S,
    pub strain: Mandel<S>,
    pub elastic: Mandel<S>,
    pub stress: Mandel<S>,
    pub prop: Props<S>,
}

#[derive(Debug, Clone)]
pub struct ElementObservables<S> {
    pub equivalent_plastic: S,
    pub equivalent_creep: S,
    pub heat_increment: S,
    pub plastic_dissipation: S,
    pub creep_dissipation: S,
    pub stored: S,
    pub elastic_energy: S,
    pub mass: S,
    pub history_energy: crate::history::HistoryEnergy<S>,
    pub polymer: Option<(MaxwellResponse<S>, S)>,
    pub backstress: Mandel<S>,
    pub yield_residual: S,
    pub thermoelastic_defect: Option<S>,
    pub stiffness: S,
}

fn mix<S: Scalar>(a: S, b: S, c: S) -> S {
    (-c + 1.0) * a + c * b
}

impl SolidModel {
    #[must_use]
    pub fn local_size(&self) -> usize {
        16 + self.internal_size
    }

    #[must_use]
    pub fn local_width(&self) -> usize {
        self.local_size() + 2 + self.forcing_channels
    }

    pub fn stiffness<S: Scalar>(&self, rho: S) -> S {
        self.penalty.apply(rho) * (1.0 - self.stiffness_floor) + self.stiffness_floor
    }

    pub fn conductivity<S: Scalar>(&self, k: S, rho: S) -> S {
        (k - self.conductivity_floor) * self.penalty.apply(rho) + self.conductivity_floor
    }

    fn endpoint_props<S: Scalar>(&self, endpoint: usize, t: S, history_state: Option<&[S]>) -> Props<S> {
        let m = &self.materials[endpoint];
        let base =
            if self.numerical { self.law.numerical_properties(m, t) } else { self.law.properties(m, t) };
        match (&self.history, history_state) {
            (Some(mh), Some(state)) => mh.properties(endpoint, state, &base),
            _ => base,
        }
    }

    pub fn properties<S: Scalar>(&self, t: S, c: S, history_state: Option<&[S]>) -> Props<S> {
        let pa = self.endpoint_props(0, t, history_state);
        let pb = self.endpoint_props(1, t, history_state);
        Props::mix(&pa, &pb, c, t)
    }

    pub fn thermal_strain<S: Scalar>(&self, t: S, c: S) -> S {
        let [a, b] = &self.materials;
        if self.numerical {
            mix(self.law.numerical_thermal_strain(a, t), self.law.numerical_thermal_strain(b, t), c)
        } else {
            mix(self.law.thermal_strain(a, t), self.law.thermal_strain(b, t), c)
        }
    }

    fn physical_state<S: Scalar>(&self, local: &[S]) -> Vec<S> {
        local[16..16 + self.internal_size].iter().zip(&self.scales).map(|(v, s)| *v * *s).collect()
    }

    fn geometry<S: Scalar>(grad0: &[[f64; 3]; 4], design: &[S]) -> ([[S; 3]; 4], S) {
        let h = [design[1] * 1e-3, design[2] * 1e-3, design[3] * 1e-3];
        let grad = std::array::from_fn(|i| std::array::from_fn(|a| S::from_f64(grad0[i][a]) / h[a]));
        (grad, h[0] * h[1] * h[2] / 6.0)
    }

    fn displacement<S: Scalar>(&self, local: &[S]) -> [[S; 3]; 4] {
        std::array::from_fn(|i| std::array::from_fn(|a| local[4 + 3 * i + a] * self.us))
    }


    #[allow(clippy::too_many_arguments)]
    fn polymer<S: Scalar>(
        &self,
        elastic: &Mandel<S>,
        state: &[S],
        old: &[S],
        t: S,
        dt: S,
        previous_elastic: &Mandel<S>,
        c: S,
    ) -> Option<MaxwellResponse<S>> {
        let visco = self.viscoelastic.as_ref()?;
        let range = self.layout.viscoelastic();
        let material = self.history.as_ref().map(|_| {
            let m = self.layout.material_start()..self.internal_size;
            (&state[m.clone()], &old[m], c)
        });
        visco.response(elastic, &state[range.clone()], &old[range], t, dt, previous_elastic, material).ok()
    }

    pub fn fields<S: Scalar>(&self, grad0: &[[f64; 3]; 4], local: &[S], design: &[S]) -> ElementFields<S> {
        let c = design[4];
        let (grad, volume) = Self::geometry(grad0, design);
        let temperature: [S; 4] = std::array::from_fn(|i| local[i] * self.ts + self.t0);
        let u = self.displacement(local);
        let state = self.physical_state(local);
        let strain = mandel::strain(&u, &grad);
        let te = (temperature[0] + temperature[1] + temperature[2] + temperature[3]) / 4.0;
        let history_state = self.history.as_ref().map(|_| &state[self.layout.material_start()..]);
        let prop = self.properties(te, c, history_state);
        let eth = self.thermal_strain(te, c);
        let zero = [S::zero(); 6];
        let plastic: Mandel<S> = if self.plastic.is_some() {
            mandel::from_slice(&state[self.layout.plastic_strain()])
        } else {
            zero
        };
        let creep: Mandel<S> =
            if self.creep.is_some() { mandel::from_slice(&state[self.layout.creep_strain()]) } else { zero };
        let elastic: Mandel<S> =
            std::array::from_fn(|i| strain[i] - eth * IDENTITY[i] - plastic[i] - creep[i]);
        let mut stress = mandel::hooke(prop.get(idx::E), prop.get(idx::NU), &elastic);
        if self.viscoelastic.is_some()
            && let Some(r) = self.polymer(&elastic, &state, &state, te, S::one(), &elastic, c)
        {
            stress = r.stress;
        }
        ElementFields { temperature, state, grad, volume, strain, elastic, stress, prop }
    }

    fn previous_props<S: Scalar>(&self, previous: &[S], c: S) -> (Props<S>, Vec<S>, [S; 4]) {
        let tp: [S; 4] = std::array::from_fn(|i| previous[i] * self.ts + self.t0);
        let old = self.physical_state(previous);
        let told = (tp[0] + tp[1] + tp[2] + tp[3]) / 4.0;
        let history_state = self.history.as_ref().map(|_| &old[self.layout.material_start()..]);
        (self.properties(told, c, history_state), old, tp)
    }

    fn previous_strain<S: Scalar>(&self, previous: &[S], grad: &[[S; 3]; 4]) -> Mandel<S> {
        mandel::strain(&self.displacement(previous), grad)
    }

    #[allow(clippy::too_many_lines)]
    pub fn residual<S: Scalar>(
        &self,
        grad0: &[[f64; 3]; 4],
        current: &[S],
        previous: &[S],
        design: &[S],
        out: &mut [S],
    ) {
        let ls = self.local_size();
        let dt = current[ls];
        let bulk = current[ls + 1];

        let forcing = &current[ls + 2..ls + 2 + self.forcing_channels];
        let density = design[0];
        let c = design[4];
        let f = self.fields(grad0, current, design);
        let (grad, volume) = (f.grad, f.volume);
        let tp: [S; 4] = std::array::from_fn(|i| previous[i] * self.ts + self.t0);
        let delta: [S; 4] = std::array::from_fn(|i| (current[i] - previous[i]) * self.ts);
        let te = f.prop.temperature;
        let (previous_prop, old, _) = self.previous_props(previous, c);
        let state = &f.state;
        let e_scale = self.ss / self.es;
        let mut stress = f.stress;
        let mut polymer = None;
        if self.viscoelastic.is_some() {
            let ep = self.previous_strain(previous, &grad);
            let tpm = (tp[0] + tp[1] + tp[2] + tp[3]) / 4.0;
            let [a, b] = &self.materials;
            let ethp = mix(self.law.thermal_strain(a, tpm), self.law.thermal_strain(b, tpm), c);
            let previous_elastic: Mandel<S> = std::array::from_fn(|i| ep[i] - ethp * IDENTITY[i]);
            if let Some(r) = self.polymer(&f.elastic, state, &old, te, dt, &previous_elastic, c) {
                stress = r.stress;
                polymer = Some(r);
            }
        }
        let internal = &mut out[16..16 + self.internal_size];
        let pr = self.layout.plastic();
        match self.plastic {
            Some(p) => p.residual(
                &stress,
                &state[pr.clone()],
                &old[pr.clone()],
                &f.prop,
                e_scale,
                &mut internal[pr.clone()],
            ),
            None => {
                for i in pr.clone() {
                    internal[i] = state[i] - old[i];
                }
            }
        }
        let cr = self.layout.creep();
        match self.creep {
            Some(law) => law.residual(
                &stress,
                &state[cr.clone()],
                &old[cr.clone()],
                &f.prop,
                dt,
                &mut internal[cr.clone()],
            ),
            None => {
                for i in cr.clone() {
                    internal[i] = state[i] - old[i];
                }
            }
        }
        let wc = self
            .creep
            .map_or(S::zero(), |law| law.dissipated_increment(&stress, &state[cr.clone()], &old[cr]));
        let stiffness = self.stiffness(density);
        let s: Mandel<S> = std::array::from_fn(|i| stiffness * stress[i]);
        let force = mandel::nodal_forces(&s, &grad, volume);
        let k = self.conductivity(f.prop.get(idx::K), density);
        let mut flux = [S::zero(); 4];
        for (i, fi) in flux.iter_mut().enumerate() {
            let mut acc = S::zero();
            for j in 0..4 {
                let gg = grad[i][0] * grad[j][0] + grad[i][1] * grad[j][1] + grad[i][2] * grad[j][2];
                acc += gg * f.temperature[j];
            }
            *fi = volume * k * acc;
        }
        let [ma, mb] = &self.materials;
        let storage: [S; 4] = if self.law.reversible_thermoelastic() {
            let ep = self.previous_strain(previous, &grad);
            MaterialLaw::entropy_storage_increment_from_delta(
                ma, mb, c, density, stiffness, &tp, &delta, &f.strain, &ep,
            )
        } else {
            std::array::from_fn(|i| {
                let (dha, dhb) = if self.numerical {
                    (
                        self.law.numerical_enthalpy_increment_from_delta(ma, tp[i], delta[i]),
                        self.law.numerical_enthalpy_increment_from_delta(mb, tp[i], delta[i]),
                    )
                } else {
                    (
                        self.law
                            .enthalpy_increment_from_delta(ma, tp[i], delta[i])
                            .unwrap_or_else(|_| S::from_f64(f64::NAN)),
                        self.law
                            .enthalpy_increment_from_delta(mb, tp[i], delta[i])
                            .unwrap_or_else(|_| S::from_f64(f64::NAN)),
                    )
                };
                density * ((-c + 1.0) * ma.density() * dha + c * mb.density() * dhb)
            })
        };
        let pq = self.plastic.map_or(S::zero(), |p| {
            let pr = self.layout.plastic();
            p.heat(&state[pr.clone()], &old[pr], &f.prop, &previous_prop)
        });
        let mut heat = stiffness * (pq + wc);
        let mut offset = self.layout.material_start();
        if let Some(r) = &polymer {
            let chemical = r.chemical_release;
            heat = heat + stiffness * (r.heat_increment - chemical) + density * chemical;
            let range = self.layout.viscoelastic();
            internal[range.clone()].copy_from_slice(&r.residual);
            offset = range.end;
        }
        if let Some(mh) = &self.history {
            let m = self.layout.material_start()..self.internal_size;
            let energy = mh.energy(&state[m.clone()], te, forcing, c);
            heat += density * energy.sensible_heat * dt;
            mh.residual(
                &state[m.clone()],
                &old[m.clone()],
                te,
                &stress,
                dt,
                forcing,
                &mut internal[offset..offset + m.len()],
            );
        }
        for (v, scale) in internal.iter_mut().zip(&self.scales) {
            *v = *v / *scale;
        }
        let thermal_scale = self.ks * self.ts * self.ls;
        let source = (heat / dt + bulk * density) * volume / 4.0;
        for i in 0..4 {
            out[i] = (flux[i] + volume / 4.0 * storage[i] / dt - source) / thermal_scale;
        }
        let force_scale = self.ss * self.ls * self.ls;
        for i in 0..4 {
            for a in 0..3 {
                out[4 + 3 * i + a] = force[i][a] / force_scale;
            }
        }
    }

    pub fn observables<S: Scalar>(
        &self,
        grad0: &[[f64; 3]; 4],
        current: &[S],
        previous: &[S],
        design: &[S],
        dt: S,
        forcing: &[f64],
    ) -> (ElementFields<S>, ElementObservables<S>) {
        let density = design[0];
        let c = design[4];
        let f = self.fields(grad0, current, design);
        let (previous_prop, old, tp) = self.previous_props(previous, c);
        let state = &f.state;
        let rf = self.stiffness(density);
        let pr = self.layout.plastic();
        let cr = self.layout.creep();
        let equivalent_plastic = if self.plastic.is_some() { state[6] } else { S::zero() };
        let equivalent_creep =
            self.layout.creep_accumulation().filter(|_| self.creep.is_some()).map_or(S::zero(), |i| state[i]);
        let wp = self
            .plastic
            .map_or(S::zero(), |p| p.dissipated_increment(&state[pr.clone()], &old[pr.clone()], &f.prop));
        let wc = self.creep.map_or(S::zero(), |law| {
            law.dissipated_increment(&f.stress, &state[cr.clone()], &old[cr.clone()])
        });
        let plastic_q = self
            .plastic
            .map_or(S::zero(), |p| p.heat(&state[pr.clone()], &old[pr.clone()], &f.prop, &previous_prop));
        let te = f.prop.temperature;
        let history_energy = self.history.as_ref().map_or(crate::history::HistoryEnergy::zero(), |mh| {
            let m = self.layout.material_start()..self.internal_size;
            let forcing: Vec<S> = forcing.iter().map(|v| S::from_f64(*v)).collect();
            mh.energy(&state[m], te, &forcing, c)
        });
        let mut polymer_heat = S::zero();
        let mut stored = mandel::dot(&f.stress, &f.elastic) * 0.5;
        let mut polymer = None;
        if self.viscoelastic.is_some() {
            let pf = self.fields(grad0, previous, design);
            if let Some(r) = self.polymer(&f.elastic, state, &old, te, dt, &pf.elastic, c) {
                let chemical = r.chemical_release;
                polymer_heat = rf * (r.heat_increment - chemical) + density * chemical;
                stored = r.mechanical_stored_energy;
                polymer = Some((r, polymer_heat));
            }
        }
        let plastic_strain: Mandel<S> = if self.plastic.is_some() {
            mandel::from_slice(&state[self.layout.plastic_strain()])
        } else {
            [S::zero(); 6]
        };
        let (backstress, yield_residual) = match self.plastic {
            Some(p) if p.has_yield_function() => {
                (p.backstress(&state[pr.clone()], &f.prop), p.yield_function(&f.stress, &state[pr], &f.prop))
            }
            _ => {
                let h = f.prop.get(idx::H_KIN) * (2.0 / 3.0);
                let back: Mandel<S> = std::array::from_fn(|i| h * plastic_strain[i]);
                let d = mandel::dev(&f.stress);
                let xi: Mandel<S> = std::array::from_fn(|i| d[i] - back[i]);
                (
                    back,
                    mandel::equivalent(&xi)
                        - f.prop.get(idx::YIELD)
                        - f.prop.get(idx::H_ISO) * equivalent_plastic,
                )
            }
        };
        let thermoelastic_defect = self.law.reversible_thermoelastic().then(|| {
            let pf = self.fields(grad0, previous, design);
            let [ma, mb] = &self.materials;
            let d = MaterialLaw::numerical_energy_defect(
                ma,
                mb,
                c,
                density,
                rf,
                &f.temperature,
                &tp,
                &f.strain,
                &pf.strain,
            );
            (d[0] + d[1] + d[2] + d[3]) / 4.0
        });
        let obs = ElementObservables {
            equivalent_plastic,
            equivalent_creep,
            heat_increment: rf * (plastic_q + wc) + polymer_heat,
            plastic_dissipation: rf * wp,
            creep_dissipation: rf * wc,
            stored,
            elastic_energy: rf * stored * f.volume,
            mass: density * f.prop.get(idx::DENSITY) * f.volume,
            history_energy,
            polymer,
            backstress,
            yield_residual,
            thermoelastic_defect,
            stiffness: rf,
        };
        (f, obs)
    }
}

#[derive(Debug, Clone)]
pub struct SolidElement {
    pub model: std::sync::Arc<SolidModel>,
    pub grad0: std::sync::Arc<Vec<[[f64; 3]; 4]>>,
}

impl LocalResidual for SolidElement {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], previous: &[S], design: &[S], out: &mut [S]) {
        self.model.residual(&self.grad0[item], current, previous, design, out);
    }
}

pub const MANDEL_SQRT2: f64 = SQRT2;
