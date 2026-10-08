// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::Value;

use implexity_ad::Scalar;
use implexity_core::pyobj::PyNum;

use super::{
    FARADAY, Kwargs, ModelOutputs, R_GAS, RuntimeModel, all_finite, pow_num, require_finite, sigmoid,
};
use crate::array::{Tensor, pairwise_sum};
use crate::model_errors::{PhysicsError, PhysicsResult};

fn err<T>(message: &str) -> PhysicsResult<T> {
    Err(PhysicsError::value(message))
}

fn det<S: Scalar>(m: &[S], d: usize) -> S {
    if d == 2 {
        m[0] * m[3] - m[1] * m[2]
    } else {
        m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FiniteStrainNeoHookean {
    pub shear_modulus: f64,
    pub bulk_modulus: f64,
    pub density_floor: f64,
}

impl RuntimeModel for FiniteStrainNeoHookean {
    const CLASS: &'static str = "FiniteStrainNeoHookean";
    const PARAMS: &'static [(&'static str, bool)] =
        &[("shear_modulus", false), ("bulk_modulus", false), ("density_floor", true)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            shear_modulus: kw.f64("shear_modulus", None)?,
            bulk_modulus: kw.f64("bulk_modulus", None)?,
            density_floor: kw.f64("density_floor", Some(1e-6))?,
        })
    }
}

impl FiniteStrainNeoHookean {

    pub fn response<S: Scalar>(&self, f: &Tensor<S>, rho: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        let (mu, k) = (self.shear_modulus, self.bulk_modulus);
        if !(mu.is_finite() && mu > 0.0 && k.is_finite() && k > 0.0) {
            return err("Moduli must be finite and positive.");
        }
        let nd = f.ndim();
        if nd < 2 || f.shape()[nd - 2] != f.shape()[nd - 1] || !(2..=3).contains(&f.shape()[nd - 1]) {
            return err("F must be a square 2D or 3D deformation gradient.");
        }
        let d = f.shape()[nd - 1];
        let j = f.map_blocks(2, &[], |m| Ok(vec![det(m, d)]))?;
        if !(self.density_floor.is_finite() && 0.0 < self.density_floor && self.density_floor <= 1.0) {
            return err("Density floor must be in (0,1].");
        }
        let df = d as f64;
        let lame = k - 2.0 * mu / df;
        let floor = self.density_floor;
        let interp = rho.map(|r| r.clip(0.0, 1.0).powi(3) * (1.0 - floor) + floor);
        let base_sigma = f.map_blocks(2, &[d, d], |m| {
            let jj = det(m, d);
            let mut out = Vec::with_capacity(d * d);
            for a in 0..d {
                for b in 0..d {
                    let mut bab = S::zero();
                    for c in 0..d {
                        bab += m[a * d + c] * m[b * d + c];
                    }
                    let delta = if a == b { 1.0 } else { 0.0 };
                    out.push(S::from_f64(mu) / jj * (bab - delta) + jj.ln() * lame / jj * delta);
                }
            }
            Ok(out)
        })?;
        let sigma = if interp.ndim() > 0 {
            interp.expand_last2().mul(&base_sigma)?
        } else {
            base_sigma.mul(&interp)?
        };
        let base_w = f.map_blocks(2, &[], |m| {
            let jj = det(m, d);
            let mut i1 = S::zero();
            for a in 0..d {
                for c in 0..d {
                    i1 += m[a * d + c] * m[a * d + c];
                }
            }
            let lj = jj.ln();
            Ok(vec![(i1 - df - lj * 2.0) * (0.5 * mu) + lj.powi(2) * (0.5 * lame)])
        })?;
        let w = interp.mul(&base_w)?;
        Ok(ModelOutputs::new()
            .with("cauchy_stress", sigma)
            .with("strain_energy_density", w)
            .with_scalar("jacobian_margin", j.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneralizedMaxwellViscoelasticity {
    pub equilibrium_modulus: f64,
    pub branch_moduli: Value,
    pub relaxation_times: Value,
}

impl RuntimeModel for GeneralizedMaxwellViscoelasticity {
    const CLASS: &'static str = "GeneralizedMaxwellViscoelasticity";
    const PARAMS: &'static [(&'static str, bool)] =
        &[("equilibrium_modulus", false), ("branch_moduli", false), ("relaxation_times", false)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            equilibrium_modulus: kw.f64("equilibrium_modulus", None)?,
            branch_moduli: kw.raw("branch_moduli").cloned().unwrap_or(Value::Null),
            relaxation_times: kw.raw("relaxation_times").cloned().unwrap_or(Value::Null),
        })
    }
}

impl GeneralizedMaxwellViscoelasticity {

    pub fn update<S: Scalar>(
        &self,
        strain: &Tensor<S>,
        branch_strains: &Tensor<S>,
        dt: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        let moduli = Tensor::from_json(&self.branch_moduli)?;
        let times = Tensor::from_json(&self.relaxation_times)?;
        if !self.equilibrium_modulus.is_finite() || self.equilibrium_modulus <= 0.0 {
            return err("Equilibrium modulus must be finite and positive.");
        }
        if moduli.ndim() != 1
            || moduli.size() == 0
            || times.shape() != moduli.shape()
            || !moduli.data().iter().all(|v| v.is_finite() && *v >= 0.0)
            || !times.data().iter().all(|v| v.is_finite() && *v > 0.0)
        {
            return err("Branches require matching finite nonnegative moduli and positive relaxation times.");
        }
        let nb = moduli.size();
        let mut want = vec![nb];
        want.extend_from_slice(strain.shape());
        if branch_strains.shape() != want.as_slice() {
            return err("Branch strains must have shape (branch_count, *strain.shape).");
        }
        if dt.ndim() != 0 {
            return err("Time increment must be scalar.");
        }
        let h = dt.at(0);
        let n = strain.size();
        let mut qn = Vec::with_capacity(nb * n);
        let mut branch_stress = vec![Vec::with_capacity(nb); n];
        let mut diss_terms = Vec::with_capacity(nb * n);
        for b in 0..nb {
            let g = moduli.at(b);
            let x = -h / times.at(b);
            let a = x.exp();
            let em1 = x.exp_m1();
            for (i, stress_terms) in branch_stress.iter_mut().enumerate() {
                let e = strain.at(i);
                let q = branch_strains.at(b * n + i);
                let qnew = a * q - em1 * e;
                qn.push(qnew);
                stress_terms.push((e - qnew) * g);
                diss_terms.push(((e - q).powi(2) - (e - qnew).powi(2)) * (0.5 * g));
            }
        }
        let stress: Vec<S> = (0..n)
            .map(|i| strain.at(i) * self.equilibrium_modulus + pairwise_sum(&branch_stress[i]))
            .collect();
        let mut shape = vec![nb];
        shape.extend_from_slice(strain.shape());
        Ok(ModelOutputs::new()
            .with("stress", Tensor::from_vec(strain.shape().to_vec(), stress)?)
            .with("branch_strain", Tensor::from_vec(shape, qn)?)
            .with_scalar("dissipation_energy_density", pairwise_sum(&diss_terms))
            .with_scalar("time_margin", h))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BiotPoromechanics {
    pub biot_coefficient: f64,
    pub biot_modulus: f64,
    pub drained_bulk_modulus: f64,
    pub viscosity: f64,
}

impl RuntimeModel for BiotPoromechanics {
    const CLASS: &'static str = "BiotPoromechanics";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("biot_coefficient", false),
        ("biot_modulus", false),
        ("drained_bulk_modulus", false),
        ("viscosity", true),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            biot_coefficient: kw.f64("biot_coefficient", None)?,
            biot_modulus: kw.f64("biot_modulus", None)?,
            drained_bulk_modulus: kw.f64("drained_bulk_modulus", None)?,
            viscosity: kw.f64("viscosity", Some(1.0))?,
        })
    }
}

impl BiotPoromechanics {

    pub fn response<S: Scalar>(
        &self,
        eps: &Tensor<S>,
        p: &Tensor<S>,
        gp: &Tensor<S>,
        permeability: &Tensor<S>,
        viscosity: Option<&Tensor<S>>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        let (a, m, k) = (self.biot_coefficient, self.biot_modulus, self.drained_bulk_modulus);
        if !all_finite(&[a, m, k, self.viscosity])
            || !(0.0..=1.0).contains(&a)
            || m.min(k).min(self.viscosity) <= 0.0
        {
            return err("Invalid Biot coefficients.");
        }
        let mu = viscosity.cloned().unwrap_or_else(|| Tensor::scalar(S::from_f64(self.viscosity)));
        let eff = eps.zip(p, |e, pp| e * k - pp * a)?;
        let zeta = eps.zip(p, |e, pp| e * a + pp / m)?;
        let mobility = permeability.div(&mu)?;
        let q = if mobility.ndim() > 0 && gp.ndim() > 0 {
            mobility.expand_last().mul(gp)?.map(|x| -x)
        } else {
            mobility.mul(gp)?.map(|x| -x)
        };
        if gp.ndim() < 1 || !(1..=3).contains(&gp.shape()[gp.ndim() - 1]) {
            return err("Pressure gradient needs a final spatial axis of length 1, 2 or 3.");
        }
        let storage = eps.zip(p, |e, pp| e.powi(2) * (0.5 * k) + pp.powi(2) * 0.5 / m)?;
        let dissipation = q.mul(gp)?.sum_last()?.map(|x| -x);
        Ok(ModelOutputs::new()
            .with("effective_stress", eff)
            .with("fluid_content", zeta)
            .with("darcy_flux", q)
            .with("stored_energy_density", storage)
            .with("darcy_dissipation_density", dissipation)
            .with_scalar("permeability_margin", permeability.min()?)
            .with_scalar("viscosity_margin", mu.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CahnHilliardPhaseField {
    pub mixing_energy: f64,
    pub gradient_energy: f64,
    pub mobility: f64,
}

impl RuntimeModel for CahnHilliardPhaseField {
    const CLASS: &'static str = "CahnHilliardPhaseField";
    const PARAMS: &'static [(&'static str, bool)] =
        &[("mixing_energy", false), ("gradient_energy", false), ("mobility", false)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            mixing_energy: kw.f64("mixing_energy", None)?,
            gradient_energy: kw.f64("gradient_energy", None)?,
            mobility: kw.f64("mobility", None)?,
        })
    }
}

impl CahnHilliardPhaseField {

    pub fn response<S: Scalar>(
        &self,
        phi: &Tensor<S>,
        laplacian_phi: &Tensor<S>,
        laplacian_mu: &Tensor<S>,
        phase_gradient: Option<&Tensor<S>>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        let Some(gp) = phase_gradient else {
            return err(
                "Cahn-Hilliard total interface energy requires phase_gradient; legacy local-only energy is not supported.",
            );
        };
        let (w, kappa, m) = (self.mixing_energy, self.gradient_energy, self.mobility);
        if !(w.is_finite() && w > 0.0 && kappa.is_finite() && kappa > 0.0 && m.is_finite() && m > 0.0) {
            return err("Cahn-Hilliard coefficients must be finite and positive.");
        }
        if gp.ndim() != phi.ndim() + 1
            || gp.shape()[..gp.ndim() - 1] != *phi.shape()
            || !(1..=3).contains(&gp.shape()[gp.ndim() - 1])
        {
            return err("phase_gradient must have shape (*phase.shape, spatial_dimension).");
        }
        let mu = phi.zip(laplacian_phi, |f, l| f * 2.0 * (-f + 1.0) * (-(f * 2.0) + 1.0) * w - l * kappa)?;
        let rate = laplacian_mu.scale(m);
        let grad2 = gp.mul(gp)?.sum_last()?;
        let energy = phi.zip(&grad2, |f, g| f.powi(2) * w * (-f + 1.0).powi(2) + g * (0.5 * kappa))?;
        Ok(ModelOutputs::new()
            .with("chemical_potential", mu)
            .with("phase_flux", rate)
            .with("interface_energy_density", energy))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraySurfaceRadiation {
    pub emissivity: f64,
    pub sigma: f64,
}

impl RuntimeModel for GraySurfaceRadiation {
    const CLASS: &'static str = "GraySurfaceRadiation";
    const PARAMS: &'static [(&'static str, bool)] = &[("emissivity", false), ("sigma", true)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self { emissivity: kw.f64("emissivity", None)?, sigma: kw.f64("sigma", Some(5.670_374_419e-8))? })
    }
}

impl GraySurfaceRadiation {

    pub fn heat_flux_out<S: Scalar>(
        &self,
        t: &Tensor<S>,
        t_surr: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        if !self.sigma.is_finite() || self.sigma <= 0.0 {
            return err("Radiation constant must be finite and positive.");
        }
        if !(0.0..=1.0).contains(&self.emissivity) {
            return err("Emissivity must lie in [0,1].");
        }
        let es = self.emissivity * self.sigma;
        let q = t.zip(t_surr, |a, b| (a.powi(4) - b.powi(4)) * es)?;
        Ok(ModelOutputs::new()
            .with("boundary_heat_flux", q)
            .with_scalar("absolute_temperature_margin", t.min()?.minimum(t_surr.min()?)))
    }
}

fn cross3<S: Scalar>(a: &[S], b: &[S]) -> [S; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EddyCurrentInduction;

impl RuntimeModel for EddyCurrentInduction {
    const CLASS: &'static str = "EddyCurrentInduction";
    const PARAMS: &'static [(&'static str, bool)] = &[];
    fn build(_kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self)
    }
}

impl EddyCurrentInduction {

    pub fn response<S: Scalar>(
        &self,
        electric_field: &Tensor<S>,
        magnetic_flux_density: &Tensor<S>,
        conductivity: &Tensor<S>,
        material_velocity: Option<&Tensor<S>>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        let zero = electric_field.map(|_| S::zero());
        let v = material_velocity.unwrap_or(&zero);
        for x in [electric_field, magnetic_flux_density, v] {
            if x.ndim() < 1 || x.shape()[x.ndim() - 1] != 3 {
                return err(
                    "Electric, magnetic and velocity inputs require a final vector axis of size three.",
                );
            }
        }
        let lead = |x: &Tensor<S>| x.shape()[..x.ndim() - 1].to_vec();
        let bcast = || -> PhysicsResult<Vec<usize>> {
            let s = crate::array::broadcast_shapes(&lead(electric_field), &lead(magnetic_flux_density))?;
            let s = crate::array::broadcast_shapes(&s, &lead(v))?;
            crate::array::broadcast_shapes(&s, conductivity.shape())
        };
        let shape = bcast().map_err(|_| {
            PhysicsError::value(
                "Induction vector batches and scalar conductivity are not broadcast-compatible",
            )
        })?;
        let mut vshape = shape.clone();
        vshape.push(3);
        let e = electric_field.broadcast_to(&vshape)?;
        let b = magnetic_flux_density.broadcast_to(&vshape)?;
        let v = v.broadcast_to(&vshape)?;
        let s = conductivity.broadcast_to(&shape)?;
        for x in [&e, &b, &s, &v] {
            require_finite(x, "Induction inputs must be finite")?;
        }
        if s.data().iter().any(|x| x.value() < 0.0) {
            return err("Electrical conductivity must be nonnegative");
        }
        let n = s.size();
        let (mut j, mut q, mut f, mut mech, mut elec) = (
            Vec::with_capacity(3 * n),
            Vec::with_capacity(n),
            Vec::with_capacity(3 * n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
        );
        for i in 0..n {
            let ei = &e.data()[3 * i..3 * i + 3];
            let bi = &b.data()[3 * i..3 * i + 3];
            let vi = &v.data()[3 * i..3 * i + 3];
            let vxb = cross3(vi, bi);
            let eff = [ei[0] + vxb[0], ei[1] + vxb[1], ei[2] + vxb[2]];
            let ji = eff.map(|x| s.at(i) * x);
            let fi = cross3(&ji, bi);
            q.push(pairwise_sum(&[ji[0] * eff[0], ji[1] * eff[1], ji[2] * eff[2]]));
            mech.push(pairwise_sum(&[fi[0] * vi[0], fi[1] * vi[1], fi[2] * vi[2]]));
            elec.push(pairwise_sum(&[ji[0] * ei[0], ji[1] * ei[1], ji[2] * ei[2]]));
            j.extend(ji);
            f.extend(fi);
        }
        let balance: Vec<S> = (0..n).map(|i| elec[i] - q[i] - mech[i]).collect();
        Ok(ModelOutputs::new()
            .with("current_density", Tensor::from_vec(vshape.clone(), j)?)
            .with("volumetric_heat_source", Tensor::from_vec(shape.clone(), q)?)
            .with("body_force_density", Tensor::from_vec(vshape, f)?)
            .with("mechanical_power_density", Tensor::from_vec(shape.clone(), mech)?)
            .with("electrical_power_density", Tensor::from_vec(shape.clone(), elec)?)
            .with("electromagnetic_energy_balance", Tensor::from_vec(shape, balance)?)
            .with_scalar("conductivity_margin", s.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NernstPlanckTransport {
    pub charge_number: f64,
}

impl RuntimeModel for NernstPlanckTransport {
    const CLASS: &'static str = "NernstPlanckTransport";
    const PARAMS: &'static [(&'static str, bool)] = &[("charge_number", false)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self { charge_number: kw.f64("charge_number", None)? })
    }
}

impl NernstPlanckTransport {

    pub fn response<S: Scalar>(
        &self,
        c: &Tensor<S>,
        gc: &Tensor<S>,
        e: &Tensor<S>,
        t: &Tensor<S>,
        d: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        if !self.charge_number.is_finite() {
            return err("Ionic charge number must be finite.");
        }
        let z = self.charge_number;
        let lift = |x: &Tensor<S>| if e.ndim() > 0 && x.ndim() > 0 { x.expand_last() } else { x.clone() };
        let (dv, cv, tv) = (lift(d), lift(c), lift(t));

        let diffusion = dv.mul(gc)?.map(|x| -x);
        let drift = dv.map(|x| x * z * FARADAY).div(&tv.map(|x| x * R_GAS))?.mul(&cv)?.mul(e)?;
        let flux = diffusion.add(&drift)?;
        let current = flux.map(|x| x * (z * FARADAY));
        Ok(ModelOutputs::new()
            .with("species_flux", flux)
            .with("current_density", current)
            .with_scalar("concentration_margin", c.min()?)
            .with_scalar("temperature_margin_K", t.min()?)
            .with_scalar("diffusivity_margin", d.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArchardWearEvolution {
    pub wear_coefficient: f64,
    pub hardness: f64,
}

impl RuntimeModel for ArchardWearEvolution {
    const CLASS: &'static str = "ArchardWearEvolution";
    const PARAMS: &'static [(&'static str, bool)] = &[("wear_coefficient", false), ("hardness", false)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self { wear_coefficient: kw.f64("wear_coefficient", None)?, hardness: kw.f64("hardness", None)? })
    }
}

impl ArchardWearEvolution {

    pub fn update<S: Scalar>(
        &self,
        contact_pressure: &Tensor<S>,
        slip: &Tensor<S>,
        dt: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        if !self.wear_coefficient.is_finite()
            || self.wear_coefficient < 0.0
            || !self.hardness.is_finite()
            || self.hardness <= 0.0
        {
            return err("Archard coefficient must be finite nonnegative and hardness finite positive.");
        }
        let p = contact_pressure.map(|x| x.max_f64(0.0));
        let v = if slip.ndim() == p.ndim() + 1 {
            let last = slip.shape()[slip.ndim() - 1];
            if slip.shape()[..slip.ndim() - 1] != *p.shape() || !(1..=3).contains(&last) {
                return err("Vector slip shape must match contact-pressure points.");
            }
            slip.reduce_last(|row| {
                let s2 = pairwise_sum(&row.iter().map(|&x| x * x).collect::<Vec<_>>());
                if s2.value() > 0.0 { s2.sqrt() } else { S::zero() }
            })?
        } else {
            if slip.shape() != p.shape() {
                return err("Scalar slip shape must match contact-pressure points.");
            }
            slip.map(Scalar::abs)
        };
        if dt.ndim() > 0 && dt.shape() != p.shape() {
            return err("Time increment must be scalar or match contact-pressure points.");
        }
        let rate = p.mul(&v)?.map(|x| x * self.wear_coefficient / self.hardness);
        let recession = rate.mul(dt)?;
        Ok(ModelOutputs::new()
            .with("geometry_recession_rate", rate)
            .with("geometry_recession", recession)
            .with_scalar("time_margin", dt.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopologyHydraulicGeometry {
    pub maximum_area: f64,
    pub maximum_hydraulic_diameter: f64,
    pub minimum_open_fraction: f64,
}

impl RuntimeModel for TopologyHydraulicGeometry {
    const CLASS: &'static str = "TopologyHydraulicGeometry";
    const PARAMS: &'static [(&'static str, bool)] =
        &[("maximum_area", false), ("maximum_hydraulic_diameter", false), ("minimum_open_fraction", true)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            maximum_area: kw.f64("maximum_area", None)?,
            maximum_hydraulic_diameter: kw.f64("maximum_hydraulic_diameter", None)?,
            minimum_open_fraction: kw.f64("minimum_open_fraction", Some(0.02))?,
        })
    }
}

impl TopologyHydraulicGeometry {

    pub fn response<S: Scalar>(&self, rho: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        let (a, dmax, fmin) =
            (self.maximum_area, self.maximum_hydraulic_diameter, self.minimum_open_fraction);
        if !all_finite(&[a, dmax, fmin]) || a.min(dmax) <= 0.0 || !(0.0 < fmin && fmin <= 1.0) {
            return err("Invalid hydraulic geometry bounds.");
        }
        let void = -rho.map(|r| r.clip(0.0, 1.0)).mean() + 1.0;
        let frac = void * (1.0 - fmin) + fmin;
        Ok(ModelOutputs::new()
            .with_scalar("hydraulic_area", frac * a)
            .with_scalar("hydraulic_diameter", frac.sqrt() * dmax)
            .with_scalar("open_fraction", frac))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HydraulicNetworkSegment {
    pub length: f64,
    pub roughness: f64,
}

impl RuntimeModel for HydraulicNetworkSegment {
    const CLASS: &'static str = "HydraulicNetworkSegment";
    const PARAMS: &'static [(&'static str, bool)] = &[("length", false), ("roughness", false)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self { length: kw.f64("length", None)?, roughness: kw.f64("roughness", None)? })
    }
}

impl HydraulicNetworkSegment {

    pub fn response<S: Scalar>(
        &self,
        mass_flow: &Tensor<S>,
        density: &Tensor<S>,
        viscosity: &Tensor<S>,
        area: &Tensor<S>,
        diameter: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        if !self.length.is_finite()
            || self.length <= 0.0
            || !self.roughness.is_finite()
            || self.roughness < 0.0
        {
            return err("Hydraulic length must be positive and roughness nonnegative, both finite.");
        }
        let v = mass_flow.div(&density.mul(area)?)?;
        let re = density.mul(&v.map(Scalar::abs))?.mul(diameter)?.div(viscosity)?;
        let rough = self.roughness;
        let f = re.zip(diameter, |r, dh| {
            let f_lam = S::from_f64(64.0) / (r + 1e-30);
            let arg = (S::from_f64(rough) / (dh * 3.7)).powf(1.11) + S::from_f64(6.9) / (r + 1e-30);
            let f_turb = (arg.log10() * -1.8).powi(-2);
            let w = sigmoid((r - 3000.0) / 400.0);
            (-w + 1.0) * f_lam + w * f_turb
        })?;
        let dp = f
            .zip(diameter, |ff, dh| ff * (S::from_f64(self.length) / dh))?
            .map(|x| x * 0.5)
            .mul(density)?
            .mul(&v)?
            .mul(&v)?;
        let pump = dp.mul(&mass_flow.map(Scalar::abs))?.div(density)?;
        Ok(ModelOutputs::new()
            .with("pressure_drop", dp)
            .with("pump_power", pump)
            .with("reynolds_number", re)
            .with_scalar("density_margin", density.min()?)
            .with_scalar("viscosity_margin", viscosity.min()?)
            .with_scalar("area_margin", area.min()?)
            .with_scalar("diameter_margin", diameter.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrthotropicHeatConduction {
    pub conductivity_tensor: Value,
    pub simp_penalty: Option<PyNum>,
}

impl RuntimeModel for OrthotropicHeatConduction {
    const CLASS: &'static str = "OrthotropicHeatConduction";
    const PARAMS: &'static [(&'static str, bool)] = &[("conductivity_tensor", false), ("simp_penalty", true)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        let simp_penalty = match kw.raw("simp_penalty") {
            None => Some(PyNum::Float(3.0)),
            Some(Value::Bool(_)) => None,
            Some(v) => PyNum::from_value(v),
        };
        Ok(Self {
            conductivity_tensor: kw.raw("conductivity_tensor").cloned().unwrap_or(Value::Null),
            simp_penalty,
        })
    }
}

impl OrthotropicHeatConduction {

    pub fn response<S: Scalar>(&self, g: &Tensor<S>, rho: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        let Some(penalty) = self.simp_penalty.filter(|p| p.is_finite() && p.as_f64() >= 1.0) else {
            return err("SIMP penalty must be a finite real scalar at least one.");
        };
        let k = Tensor::from_json(&self.conductivity_tensor)?;
        let d = g.shape().last().copied().unwrap_or(0);
        if g.ndim() < 1 || !(1..=3).contains(&d) || k.ndim() < 2 || k.shape()[k.ndim() - 2..] != [d, d] {
            return err("Conductivity tensor dimension must match gradient.");
        }
        let lead = &g.shape()[..g.ndim() - 1];
        if k.ndim() != 2 && k.shape()[..k.ndim() - 2] != *lead {
            return err("Conductivity tensor points must match gradient points.");
        }
        if rho.ndim() > 0 && rho.shape() != lead {
            return err("Topology points must match gradient points.");
        }
        let interp = rho.map(|r| pow_num(r.clip(0.0, 1.0), penalty));
        let npts: usize = lead.iter().product();
        let per_point = k.ndim() != 2;
        let mut kg = Vec::with_capacity(npts * d);
        for p in 0..npts {
            let kb = if per_point { p * d * d } else { 0 };
            for i in 0..d {
                let terms: Vec<S> = (0..d).map(|j| g.at(p * d + j) * k.at(kb + i * d + j)).collect();
                kg.push(pairwise_sum(&terms));
            }
        }
        let kg = Tensor::from_vec(g.shape().to_vec(), kg)?;
        let q = interp.expand_last().mul(&kg)?.map(|x| -x);
        let nk: usize = k.shape()[..k.ndim() - 2].iter().product();
        let mut lam_min = f64::INFINITY;
        let mut asym_max: f64 = 0.0;
        for p in 0..nk.max(1) {
            let block: Vec<f64> = k.data()[p * d * d..(p + 1) * d * d].to_vec();
            let sym: Vec<f64> =
                (0..d * d).map(|ij| 0.5 * (block[ij] + block[(ij % d) * d + ij / d])).collect();
            let lam =
                implexity_ad::small::eigvalsh(&sym, d).map_err(|e| PhysicsError::value(e.to_string()))?;
            lam_min = lam.iter().fold(lam_min, |acc, v| acc.min(*v));
            let scale = block.iter().fold(0.0_f64, |acc, v| acc.max(v.abs())).max(f64::MIN_POSITIVE);
            let asym =
                (0..d * d).map(|ij| (block[ij] - block[(ij % d) * d + ij / d]).abs()).fold(0.0_f64, f64::max);
            asym_max = asym_max.max(asym / scale);
        }
        Ok(ModelOutputs::new()
            .with("heat_flux", q)
            .with_scalar("conductivity_eigenvalue_margin", S::from_f64(lam_min))
            .with_scalar("conductivity_symmetry_margin", S::from_f64(1e-12 - asym_max)))
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IsotropicHeatConduction;

impl RuntimeModel for IsotropicHeatConduction {
    const CLASS: &'static str = "IsotropicHeatConduction";
    const PARAMS: &'static [(&'static str, bool)] = &[];
    fn build(_kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self)
    }
}

impl IsotropicHeatConduction {

    pub fn response<S: Scalar>(&self, g: &Tensor<S>, k: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        let q = if g.ndim() > 0 && k.ndim() > 0 { k.expand_last().mul(g)? } else { k.mul(g)? }.map(|x| -x);
        Ok(ModelOutputs::new().with("heat_flux", q).with_scalar("conductivity_margin", k.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IsotropicSmallStrainElasticity {
    pub poisson_ratio: f64,
}

impl RuntimeModel for IsotropicSmallStrainElasticity {
    const CLASS: &'static str = "IsotropicSmallStrainElasticity";
    const PARAMS: &'static [(&'static str, bool)] = &[("poisson_ratio", false)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self { poisson_ratio: kw.f64("poisson_ratio", None)? })
    }
}

impl IsotropicSmallStrainElasticity {

    pub fn response<S: Scalar>(
        &self,
        strain: &Tensor<S>,
        e_mod: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        let nu = self.poisson_ratio;
        if !(-1.0 < nu && nu < 0.5) {
            return err("Poisson ratio must lie in (-1,0.5).");
        }
        let nd = strain.ndim();
        if nd < 2
            || strain.shape()[nd - 2] != strain.shape()[nd - 1]
            || !(2..=3).contains(&strain.shape()[nd - 1])
        {
            return err("Strain must be a 2D plane-strain or 3D tensor.");
        }
        let d = strain.shape()[nd - 1];
        let sym = strain.map_blocks(2, &[d, d], |m| {
            Ok((0..d * d).map(|ij| (m[ij] + m[(ij % d) * d + ij / d]) * 0.5).collect())
        })?;
        let mu = e_mod.map(|e| e / (2.0 * (1.0 + nu)));
        let lam = e_mod.map(|e| e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu)));
        let tr =
            sym.map_blocks(2, &[], |m| Ok(vec![(0..d).map(|i| m[i * d + i]).fold(S::zero(), |a, b| a + b)]))?;
        let eye = Tensor::from_vec(
            vec![d, d],
            (0..d * d).map(|ij| S::from_f64(if ij % (d + 1) == 0 { 1.0 } else { 0.0 })).collect(),
        )?;
        let lm = if lam.ndim() > 0 { lam.expand_last2() } else { lam.clone() };
        let mm = if mu.ndim() > 0 { mu.expand_last2() } else { mu.clone() };
        let tt = if tr.ndim() > 0 { tr.expand_last2() } else { tr.clone() };
        let sigma = lm.mul(&tt)?.mul(&eye)?.add(&mm.map(|x| x * 2.0).mul(&sym)?)?;
        let w = sym.mul(&sigma)?.map_blocks(2, &[], |m| Ok(vec![pairwise_sum(m) * 0.5]))?;
        Ok(ModelOutputs::new()
            .with("stress", sigma)
            .with("strain_energy_density", w)
            .with_scalar("modulus_margin", e_mod.min()?))
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct InertialBodyForce;

impl RuntimeModel for InertialBodyForce {
    const CLASS: &'static str = "InertialBodyForce";
    const PARAMS: &'static [(&'static str, bool)] = &[];
    fn build(_kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self)
    }
}

impl InertialBodyForce {

    pub fn response<S: Scalar>(&self, rho: &Tensor<S>, a: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        let f = if a.ndim() > 0 && rho.ndim() > 0 { rho.expand_last().mul(a)? } else { rho.mul(a)? };
        Ok(ModelOutputs::new()
            .with("inertial_body_force_density", f)
            .with_scalar("density_margin", rho.min()?))
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThermalStorageRate;

impl RuntimeModel for ThermalStorageRate {
    const CLASS: &'static str = "ThermalStorageRate";
    const PARAMS: &'static [(&'static str, bool)] = &[];
    fn build(_kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self)
    }
}

impl ThermalStorageRate {

    pub fn response<S: Scalar>(&self, c: &Tensor<S>, rate: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        Ok(ModelOutputs::new()
            .with("thermal_storage_rate", c.mul(rate)?)
            .with_scalar("heat_capacity_margin", c.min()?))
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DensityHeatCapacity;

impl RuntimeModel for DensityHeatCapacity {
    const CLASS: &'static str = "DensityHeatCapacity";
    const PARAMS: &'static [(&'static str, bool)] = &[];
    fn build(_kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self)
    }
}

impl DensityHeatCapacity {

    pub fn response<S: Scalar>(&self, rho: &Tensor<S>, cp: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        if rho.ndim() > 0 && cp.ndim() > 0 && rho.shape() != cp.shape() {
            return err("Density and heat capacity must be scalar or share point shape.");
        }
        Ok(ModelOutputs::new()
            .with("volumetric_heat_capacity", rho.mul(cp)?)
            .with_scalar("density_margin", rho.min()?)
            .with_scalar("specific_heat_capacity_margin", cp.min()?))
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnthalpyStorageIncrement;

impl RuntimeModel for EnthalpyStorageIncrement {
    const CLASS: &'static str = "EnthalpyStorageIncrement";
    const PARAMS: &'static [(&'static str, bool)] = &[];
    fn build(_kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self)
    }
}

impl EnthalpyStorageIncrement {

    pub fn response<S: Scalar>(
        &self,
        h: &Tensor<S>,
        old: &Tensor<S>,
        rho: &Tensor<S>,
        dt: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        if h.shape() != old.shape() {
            return err("Current and previous enthalpy must share point shape.");
        }
        if [rho, dt].iter().any(|x| x.ndim() > 0 && x.shape() != h.shape()) {
            return err("Reference density and time step must be scalar or match enthalpy points.");
        }
        let increment = rho.mul(&h.sub(old)?)?;
        let rate = increment.div(dt)?;
        Ok(ModelOutputs::new()
            .with("stored_energy_increment", increment)
            .with("thermal_storage_rate", rate)
            .with_scalar("reference_density_margin", rho.min()?)
            .with_scalar("time_step_margin_s", dt.min()?))
    }
}
