// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::Value;

use implexity_ad::Scalar;
use implexity_core::pyobj::PyNum;

use super::{
    FARADAY, Kwargs, ModelOutputs, R_GAS, RuntimeModel, all_finite, pow_num, require_finite, sigmoid,
    softplus, valid_temperature,
};
use crate::array::Tensor;
use crate::model_errors::{PhysicsError, PhysicsResult};

fn err<T>(message: &str) -> PhysicsResult<T> {
    Err(PhysicsError::value(message))
}

fn array_param(value: Option<&Value>, message: &str) -> PhysicsResult<Tensor<f64>> {
    match value {
        Some(v) => Tensor::from_json(v).map_err(|_| PhysicsError::value(message)),
        None => err(message),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TemperatureDependentSolidMaterial {
    pub t_ref: f64,
    pub t_min: f64,
    pub t_max: f64,
    pub e_ref: f64,
    pub dln_e_dt: f64,
    pub nu: f64,
    pub alpha_ref: f64,
    pub dalpha_dt: f64,
    pub k_ref: f64,
    pub dlnk_dt: f64,
    pub cp_ref: f64,
    pub dlncp_dt: f64,
    pub density: f64,
    pub simp_penalty: PyNum,
    pub property_floor: f64,
}

impl RuntimeModel for TemperatureDependentSolidMaterial {
    const CLASS: &'static str = "TemperatureDependentSolidMaterial";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("T_ref", false),
        ("T_min", false),
        ("T_max", false),
        ("E_ref", false),
        ("dlnE_dT", false),
        ("nu", false),
        ("alpha_ref", false),
        ("dalpha_dT", false),
        ("k_ref", false),
        ("dlnk_dT", false),
        ("cp_ref", false),
        ("dlncp_dT", false),
        ("density", false),
        ("simp_penalty", true),
        ("property_floor", true),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            t_ref: kw.f64("T_ref", None)?,
            t_min: kw.f64("T_min", None)?,
            t_max: kw.f64("T_max", None)?,
            e_ref: kw.f64("E_ref", None)?,
            dln_e_dt: kw.f64("dlnE_dT", None)?,
            nu: kw.f64("nu", None)?,
            alpha_ref: kw.f64("alpha_ref", None)?,
            dalpha_dt: kw.f64("dalpha_dT", None)?,
            k_ref: kw.f64("k_ref", None)?,
            dlnk_dt: kw.f64("dlnk_dT", None)?,
            cp_ref: kw.f64("cp_ref", None)?,
            dlncp_dt: kw.f64("dlncp_dT", None)?,
            density: kw.f64("density", None)?,
            simp_penalty: kw.num("simp_penalty", Some(PyNum::Float(3.0)))?,
            property_floor: kw.f64("property_floor", Some(1e-6))?,
        })
    }
}

impl TemperatureDependentSolidMaterial {

    pub fn validate(&self) -> PhysicsResult<()> {
        let vals = [
            self.t_ref,
            self.t_min,
            self.t_max,
            self.e_ref,
            self.dln_e_dt,
            self.nu,
            self.alpha_ref,
            self.dalpha_dt,
            self.k_ref,
            self.dlnk_dt,
            self.cp_ref,
            self.dlncp_dt,
            self.density,
            self.simp_penalty.as_f64(),
            self.property_floor,
        ];
        if !all_finite(&vals) {
            return err("Material parameters must be finite.");
        }
        if !(0.0 < self.t_min && self.t_min < self.t_ref && self.t_ref < self.t_max) {
            return err("0 < T_min < T_ref < T_max is required.");
        }
        if self.e_ref <= 0.0 || self.k_ref <= 0.0 || self.cp_ref <= 0.0 || self.density <= 0.0 {
            return err("E, k, cp and density must be positive.");
        }
        if !(-1.0 < self.nu && self.nu < 0.5) {
            return err("Poisson ratio must satisfy -1 < nu < 0.5.");
        }
        if self.simp_penalty.as_f64() < 1.0 {
            return err("SIMP penalty must be at least one.");
        }
        if !(0.0 < self.property_floor && self.property_floor <= 1.0) {
            return err("Property floor must be in (0,1].");
        }
        Ok(())
    }


    pub fn properties<S: Scalar>(&self, t: &Tensor<S>, rho: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        self.validate()?;
        let dt = t.map(|x| x - self.t_ref);
        let floor = self.property_floor;
        let interp = rho.map(|r| pow_num(r.clip(0.0, 1.0), self.simp_penalty) * (1.0 - floor) + floor);
        let e = dt.map(|d| (d * self.dln_e_dt).exp() * self.e_ref).mul(&interp)?;
        let k = dt.map(|d| (d * self.dlnk_dt).exp() * self.k_ref).mul(&interp)?;
        let cp = dt.map(|d| (d * self.dlncp_dt).exp() * self.cp_ref);
        let alpha = dt.map(|d| (d * self.dalpha_dt + 1.0) * self.alpha_ref);
        let density = interp.scale(self.density);
        let volumetric = density.mul(&cp)?;
        Ok(ModelOutputs::new()
            .with("youngs_modulus", e)
            .with("thermal_conductivity", k)
            .with("heat_capacity", cp)
            .with("thermal_expansion", alpha)
            .with("mass_density", density)
            .with("volumetric_heat_capacity", volumetric)
            .with_scalar("validity_margin_K", valid_temperature(t, self.t_min, self.t_max)?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalConvection {
    pub h: f64,
    pub ambient_temperature: f64,
}

impl RuntimeModel for ExternalConvection {
    const CLASS: &'static str = "ExternalConvection";
    const PARAMS: &'static [(&'static str, bool)] = &[("h", false), ("ambient_temperature", false)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self { h: kw.f64("h", None)?, ambient_temperature: kw.f64("ambient_temperature", None)? })
    }
}

impl ExternalConvection {

    pub fn validate(&self) -> PhysicsResult<()> {
        if !self.h.is_finite() || self.h <= 0.0 {
            return err("Convective heat-transfer coefficient must be positive.");
        }
        if !self.ambient_temperature.is_finite() || self.ambient_temperature <= 0.0 {
            return err("Ambient absolute temperature must be positive.");
        }
        Ok(())
    }


    pub fn heat_flux_out<S: Scalar>(&self, surface_temperature: &Tensor<S>) -> PhysicsResult<Tensor<S>> {
        self.validate()?;
        Ok(surface_temperature.map(|t| (t - self.ambient_temperature) * self.h))
    }


    pub fn heat_rate_out<S: Scalar>(
        &self,
        surface_temperature: &Tensor<S>,
        area: &Tensor<S>,
    ) -> PhysicsResult<Tensor<S>> {
        self.heat_flux_out(surface_temperature)?.mul(area)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TemperatureDependentViscosity {
    pub law: String,
    pub mu_ref: f64,
    pub t_ref: f64,
    pub t_min: f64,
    pub t_max: f64,
    pub activation_over_r: f64,
    pub sutherland_constant: f64,
    numeric_ok: bool,
}

impl RuntimeModel for TemperatureDependentViscosity {
    const CLASS: &'static str = "TemperatureDependentViscosity";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("law", false),
        ("mu_ref", false),
        ("T_ref", false),
        ("T_min", false),
        ("T_max", false),
        ("activation_over_R", true),
        ("sutherland_constant", true),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        let names = ["mu_ref", "T_ref", "T_min", "T_max", "activation_over_R", "sutherland_constant"];

        let numeric_ok = names.iter().all(|n| kw.raw(n).is_none_or(Value::is_number));
        let num = |n: &str, d: Option<f64>| -> f64 { kw.f64(n, d).unwrap_or(f64::NAN) };
        if kw.raw("law").is_none() {
            return err("missing numerical authoring ['law']");
        }
        Ok(Self {
            law: kw.text("law", None)?,
            mu_ref: num("mu_ref", None),
            t_ref: num("T_ref", None),
            t_min: num("T_min", None),
            t_max: num("T_max", None),
            activation_over_r: num("activation_over_R", Some(0.0)),
            sutherland_constant: num("sutherland_constant", Some(0.0)),
            numeric_ok,
        })
    }
}

impl TemperatureDependentViscosity {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        law: &str,
        mu_ref: f64,
        t_ref: f64,
        t_min: f64,
        t_max: f64,
        activation_over_r: f64,
        sutherland: f64,
    ) -> Self {
        Self {
            law: law.into(),
            mu_ref,
            t_ref,
            t_min,
            t_max,
            activation_over_r,
            sutherland_constant: sutherland,
            numeric_ok: true,
        }
    }


    pub fn validate(&self) -> PhysicsResult<()> {
        if self.law != "arrhenius_liquid" && self.law != "sutherland_gas" {
            return err("Unknown viscosity law.");
        }
        let vals = [
            self.mu_ref,
            self.t_ref,
            self.t_min,
            self.t_max,
            self.activation_over_r,
            self.sutherland_constant,
        ];
        if !self.numeric_ok || !all_finite(&vals) || self.t_min <= 0.0 {
            return err(
                "Viscosity coefficients must be finite real scalars with positive temperature bounds.",
            );
        }
        if self.mu_ref <= 0.0 || self.t_ref <= 0.0 || !(self.t_min < self.t_ref && self.t_ref < self.t_max) {
            return err("Invalid viscosity reference or range.");
        }
        if self.law == "sutherland_gas" && self.sutherland_constant <= -self.t_min {
            return err("Sutherland denominator may not vanish in the validity range.");
        }
        Ok(())
    }


    pub fn viscosity<S: Scalar>(&self, t: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        self.validate()?;
        let mu = if self.law == "arrhenius_liquid" {
            t.map(|x| ((x.recip() - 1.0 / self.t_ref) * self.activation_over_r).exp() * self.mu_ref)
        } else {
            let s = self.sutherland_constant;
            t.map(|x| (x / self.t_ref).powf(1.5) * self.mu_ref * (self.t_ref + s) / (x + s))
        };
        Ok(ModelOutputs::new()
            .with("dynamic_viscosity", mu)
            .with_scalar("validity_margin_K", valid_temperature(t, self.t_min, self.t_max)?))
    }
}

fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[derive(Debug, Clone, PartialEq)]
pub struct PressureStructureTransfer {
    pub mapping: Option<Value>,
    pub quadrature_areas_m2: Option<Value>,
    pub fluid_geometry_mode: String,
    pub structural_points_m: Option<Value>,
    pub quadrature_points_m: Option<Value>,
}

impl RuntimeModel for PressureStructureTransfer {
    const CLASS: &'static str = "PressureStructureTransfer";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("mapping", false),
        ("quadrature_areas_m2", true),
        ("fluid_geometry_mode", true),
        ("structural_points_m", true),
        ("quadrature_points_m", true),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            mapping: kw.raw("mapping").cloned(),
            quadrature_areas_m2: kw.any("quadrature_areas_m2"),
            fluid_geometry_mode: kw.text("fluid_geometry_mode", Some("frozen"))?,
            structural_points_m: kw.any("structural_points_m"),
            quadrature_points_m: kw.any("quadrature_points_m"),
        })
    }
}

struct TransferData {
    m: Tensor<f64>,
    a: Tensor<f64>,
}

impl PressureStructureTransfer {
    fn geometry(&self, m: &Tensor<f64>, a: &Tensor<f64>) -> PhysicsResult<()> {
        let (Some(xs_raw), Some(xq_raw)) = (&self.structural_points_m, &self.quadrature_points_m) else {
            return err(
                "Conservative pressure transfer requires explicit structural_points_m and quadrature_points_m; coordinates cannot be inferred from a load map.",
            );
        };
        let shape_msg = "Pressure map requires finite Cartesian nodal coordinates (nodes,3), quadrature coordinates (faces,3), and node-major xyz force rows.";
        let xs = Tensor::from_json(xs_raw).map_err(|_| PhysicsError::value(shape_msg))?;
        let xq = Tensor::from_json(xq_raw).map_err(|_| PhysicsError::value(shape_msg))?;
        let faces = a.size();
        if xs.ndim() != 2
            || xs.shape()[1] != 3
            || xs.shape()[0] == 0
            || xq.shape() != [faces, 3]
            || m.shape() != [3 * xs.shape()[0], 3 * faces]
            || !xs.data().iter().all(Scalar::is_finite)
            || !xq.data().iter().all(Scalar::is_finite)
        {
            return err(shape_msg);
        }
        let nodes = xs.shape()[0];
        let mut origin = [0.0; 3];
        for (c, o) in origin.iter_mut().enumerate() {
            let col: Vec<f64> = (0..nodes).map(|i| xs.at(3 * i + c)).collect();
            *o = crate::array::pairwise_sum(&col) / nodes as f64;
        }
        let centred = |t: &Tensor<f64>, n: usize| -> Vec<[f64; 3]> {
            (0..n)
                .map(|i| [t.at(3 * i) - origin[0], t.at(3 * i + 1) - origin[1], t.at(3 * i + 2) - origin[2]])
                .collect()
        };
        let ps = centred(&xs, nodes);
        let pq = centred(&xq, faces);
        let cols = 3 * faces;

        let mut force_error: f64 = 0.0;
        let mut moment_error: f64 = 0.0;
        for mode in 0..6 {
            let mode_of = |p: [f64; 3], comp: usize| -> f64 {
                if mode < 3 {
                    if comp == mode { 1.0 } else { 0.0 }
                } else {
                    let mut axis = [0.0; 3];
                    axis[mode - 3] = 1.0;
                    cross(axis, p)[comp]
                }
            };
            for j in 0..cols {
                let mut acc = 0.0;
                for (i, p) in ps.iter().enumerate() {
                    for comp in 0..3 {
                        acc += m.at((3 * i + comp) * cols + j) * mode_of(*p, comp);
                    }
                }
                let target = mode_of(pq[j / 3], j % 3);
                let e = (acc - target).abs();
                if mode < 3 {
                    force_error = if e.is_nan() { f64::NAN } else { force_error.max(e) };
                } else {
                    moment_error = if e.is_nan() { f64::NAN } else { moment_error.max(e) };
                }
            }
        }
        let scale = ps.iter().chain(&pq).flat_map(|p| p.iter()).fold(0.0_f64, |acc, v| acc.max(v.abs()));
        if !(force_error.is_finite() && moment_error.is_finite() && scale.is_finite())
            || force_error > 1e-10
            || moment_error > 1e-10 * scale
        {
            return err(
                "Pressure map does not preserve rigid translations and rotations; resultant force or moment would not be conserved.",
            );
        }
        Ok(())
    }

    fn data<S: Scalar>(&self, pressure: &Tensor<S>, normals: &Tensor<S>) -> PhysicsResult<TransferData> {
        if self.fluid_geometry_mode != "frozen" {
            return err("This pressure map is frozen geometry, not a two-way FSI provider.");
        }
        let Some(areas) = &self.quadrature_areas_m2 else {
            return err(
                "Pressure-to-force transfer requires explicit quadrature_areas_m2; legacy area-free mapping is dimensionally incomplete.",
            );
        };
        let bad = "Pressure map requires finite matrix and positive face areas.";
        let m = array_param(self.mapping.as_ref(), bad)?;
        let a = Tensor::from_json(areas).map_err(|_| PhysicsError::value(bad))?;
        if a.ndim() != 1
            || a.size() == 0
            || !a.data().iter().all(|v| v.is_finite() && *v > 0.0)
            || m.ndim() != 2
            || m.shape()[1] != 3 * a.size()
            || !m.data().iter().all(Scalar::is_finite)
        {
            return err(bad);
        }
        self.geometry(&m, &a)?;
        if pressure.shape() != a.shape() || normals.shape() != [a.size(), 3] {
            return err("Pressure and unit normals must match quadrature faces.");
        }
        let unit = normals.data().chunks(3).all(|n| {
            let norm =
                (n[0].value() * n[0].value() + n[1].value() * n[1].value() + n[2].value() * n[2].value())
                    .sqrt();
            (norm - 1.0).abs() <= 1e-8
        });
        if !pressure.data().iter().all(Scalar::is_finite)
            || !normals.data().iter().all(Scalar::is_finite)
            || !unit
        {
            return err("Pressure and normals must be finite and normals must have unit length.");
        }
        Ok(TransferData { m, a })
    }

    fn face_loads<S: Scalar>(d: &TransferData, pressure: &Tensor<S>, normals: &Tensor<S>) -> Vec<S> {
        let mut local = Vec::with_capacity(3 * d.a.size());
        for f in 0..d.a.size() {
            let pa = pressure.at(f) * d.a.at(f);
            for c in 0..3 {
                local.push(-(pa * normals.at(3 * f + c)));
            }
        }
        local
    }

    fn matvec<S: Scalar>(m: &Tensor<f64>, v: &[S]) -> Vec<S> {
        let cols = m.shape()[1];
        (0..m.shape()[0])
            .map(|r| {
                let mut acc = S::zero();
                for (j, &x) in v.iter().enumerate() {
                    acc += x * m.at(r * cols + j);
                }
                acc
            })
            .collect()
    }

    fn rmatvec<S: Scalar>(m: &Tensor<f64>, v: &[S]) -> Vec<S> {
        let cols = m.shape()[1];
        (0..cols)
            .map(|j| {
                let mut acc = S::zero();
                for (r, &x) in v.iter().enumerate() {
                    acc += x * m.at(r * cols + j);
                }
                acc
            })
            .collect()
    }


    pub fn transfer<S: Scalar>(&self, pressure: &Tensor<S>, normals: &Tensor<S>) -> PhysicsResult<Tensor<S>> {
        let d = self.data(pressure, normals)?;
        Ok(Tensor::vector(Self::matvec(&d.m, &Self::face_loads(&d, pressure, normals))))
    }

    fn velocity<S: Scalar>(value: &Tensor<S>, len: usize, name: &str) -> PhysicsResult<()> {
        if value.shape() != [len] {
            return err(&format!("{name} must be a real velocity array with shape ({len},)."));
        }
        require_finite(value, &format!("{name} must be finite."))
    }


    pub fn normal_velocity<S: Scalar>(
        &self,
        pressure: &Tensor<S>,
        normals: &Tensor<S>,
        structural_velocity: &Tensor<S>,
    ) -> PhysicsResult<Tensor<S>> {
        let d = self.data(pressure, normals)?;
        Self::velocity(structural_velocity, d.m.shape()[0], "Node-major structural velocity")?;
        let face = Self::rmatvec(&d.m, structural_velocity.data());
        Ok(Tensor::vector(
            (0..d.a.size())
                .map(|f| {
                    crate::array::pairwise_sum(&[0, 1, 2].map(|c| face[3 * f + c] * normals.at(3 * f + c)))
                })
                .collect(),
        ))
    }


    pub fn validity<S: Scalar>(
        &self,
        pressure: &Tensor<S>,
        normals: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        self.data(pressure, normals)?;
        let dev = normals.reduce_last(|n| {
            let sq = crate::array::pairwise_sum(&[n[0] * n[0], n[1] * n[1], n[2] * n[2]]);
            (sq.sqrt() - 1.0).abs()
        })?;
        Ok(ModelOutputs::new().with_scalar("normal_unit_margin", -dev.max()? + 1e-8))
    }


    pub fn virtual_work_residual<S: Scalar>(
        &self,
        pressure: &Tensor<S>,
        normals: &Tensor<S>,
        structural_velocity: &Tensor<S>,
        fluid_normal_velocity: &Tensor<S>,
    ) -> PhysicsResult<S> {
        let d = self.data(pressure, normals)?;
        Self::velocity(structural_velocity, d.m.shape()[0], "Node-major structural velocity")?;
        Self::velocity(fluid_normal_velocity, d.a.size(), "Face-normal velocity")?;
        let f = Self::matvec(&d.m, &Self::face_loads(&d, pressure, normals));
        let fv: Vec<S> = f.iter().zip(structural_velocity.data()).map(|(&a, &b)| a * b).collect();
        let pv: Vec<S> =
            (0..d.a.size()).map(|i| pressure.at(i) * d.a.at(i) * fluid_normal_velocity.at(i)).collect();
        Ok(crate::array::pairwise_sum(&fv) + crate::array::pairwise_sum(&pv))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegularisedContactFriction {
    pub normal_penalty: f64,
    pub gap_width: f64,
    pub friction_coefficient: f64,
    pub slip_regularisation: f64,
    pub heat_fraction_a: f64,
}

impl RuntimeModel for RegularisedContactFriction {
    const CLASS: &'static str = "RegularisedContactFriction";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("normal_penalty", false),
        ("gap_width", false),
        ("friction_coefficient", false),
        ("slip_regularisation", false),
        ("heat_fraction_a", true),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            normal_penalty: kw.f64("normal_penalty", None)?,
            gap_width: kw.f64("gap_width", None)?,
            friction_coefficient: kw.f64("friction_coefficient", None)?,
            slip_regularisation: kw.f64("slip_regularisation", None)?,
            heat_fraction_a: kw.f64("heat_fraction_a", Some(0.5))?,
        })
    }
}

impl RegularisedContactFriction {

    pub fn validate(&self) -> PhysicsResult<()> {
        let v = [
            self.normal_penalty,
            self.gap_width,
            self.friction_coefficient,
            self.slip_regularisation,
            self.heat_fraction_a,
        ];
        if !all_finite(&v) {
            return err("Contact parameters must be finite.");
        }
        if self.normal_penalty <= 0.0 || self.gap_width <= 0.0 || self.slip_regularisation <= 0.0 {
            return err("Contact regularisation parameters must be positive.");
        }
        if self.friction_coefficient < 0.0 {
            return err("Friction coefficient must be non-negative.");
        }
        if !(0.0..=1.0).contains(&self.heat_fraction_a) {
            return err("Heat split must be between zero and one.");
        }
        Ok(())
    }


    pub fn response<S: Scalar>(&self, gap: &Tensor<S>, slip: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        self.validate()?;
        let pressure = gap.map(|g| softplus(-g, self.gap_width) * self.normal_penalty);
        let (tangential, dissipation) = if slip.ndim() == gap.ndim() + 1 {
            let last = slip.shape()[slip.ndim() - 1];
            if slip.shape()[..slip.ndim() - 1] != *gap.shape() || !(1..=3).contains(&last) {
                return err("Vector slip shape must match gap points.");
            }
            let eps2 = self.slip_regularisation * self.slip_regularisation;
            let speed = slip.reduce_last(|v| {
                (crate::array::pairwise_sum(&v.iter().map(|&x| x * x).collect::<Vec<_>>()) + eps2).sqrt()
            })?;
            let factor = pressure.zip(&speed, |p, s| p * (-self.friction_coefficient) / s)?;
            let tangential = factor.expand_last().mul(slip)?;
            let dissipation = tangential.mul(slip)?.sum_last()?.map(|x| -x);
            (tangential, dissipation)
        } else {
            if slip.shape() != gap.shape() {
                return err("Scalar slip shape must match gap points.");
            }
            let tangential = pressure
                .zip(slip, |p, v| p * (-self.friction_coefficient) * (v / self.slip_regularisation).tanh())?;
            let dissipation = tangential.zip(slip, |t, v| -t * v)?;
            (tangential, dissipation)
        };
        let fa = self.heat_fraction_a;
        Ok(ModelOutputs::new()
            .with("contact_pressure", pressure)
            .with("tangential_traction", tangential)
            .with("frictional_heat_a", dissipation.scale(fa))
            .with("frictional_heat_b", dissipation.scale(1.0 - fa))
            .with("frictional_dissipation", dissipation))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegularisedJ2PlasticDamage {
    pub youngs_modulus: f64,
    pub poisson_ratio: f64,
    pub yield_stress: f64,
    pub hardening_modulus: f64,
    pub transition_width: f64,
    pub damage_onset: f64,
    pub damage_rate: f64,
    numeric_ok: bool,
}

pub const J2_DISSIPATION_STATUS: &str =
    "unavailable: previous total strain and thermodynamic damage/regularization accounting required";

impl RuntimeModel for RegularisedJ2PlasticDamage {
    const CLASS: &'static str = "RegularisedJ2PlasticDamage";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("youngs_modulus", false),
        ("poisson_ratio", false),
        ("yield_stress", false),
        ("hardening_modulus", false),
        ("transition_width", false),
        ("damage_onset", false),
        ("damage_rate", false),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        let numeric_ok = Self::PARAMS.iter().all(|(n, _)| kw.raw(n).is_some_and(Value::is_number));
        let num = |n: &str| kw.f64(n, None).unwrap_or(f64::NAN);
        Ok(Self {
            youngs_modulus: num("youngs_modulus"),
            poisson_ratio: num("poisson_ratio"),
            yield_stress: num("yield_stress"),
            hardening_modulus: num("hardening_modulus"),
            transition_width: num("transition_width"),
            damage_onset: num("damage_onset"),
            damage_rate: num("damage_rate"),
            numeric_ok,
        })
    }
}

fn dev6<S: Scalar>(x: &[S; 6]) -> [S; 6] {
    let m = (x[0] + x[1] + x[2]) / 3.0;
    [x[0] - m, x[1] - m, x[2] - m, x[3], x[4], x[5]]
}

impl RegularisedJ2PlasticDamage {
    #[must_use]
    pub fn new(
        e: f64,
        nu: f64,
        yield_stress: f64,
        hardening: f64,
        width: f64,
        onset: f64,
        rate: f64,
    ) -> Self {
        Self {
            youngs_modulus: e,
            poisson_ratio: nu,
            yield_stress,
            hardening_modulus: hardening,
            transition_width: width,
            damage_onset: onset,
            damage_rate: rate,
            numeric_ok: true,
        }
    }


    pub fn validate(&self) -> PhysicsResult<()> {
        let v = [
            self.youngs_modulus,
            self.poisson_ratio,
            self.yield_stress,
            self.hardening_modulus,
            self.transition_width,
            self.damage_onset,
            self.damage_rate,
        ];
        if !self.numeric_ok || !all_finite(&v) {
            return err("J2 material parameters must be finite real scalars.");
        }
        if self.youngs_modulus <= 0.0
            || self.yield_stress <= 0.0
            || self.hardening_modulus < 0.0
            || self.transition_width <= 0.0
        {
            return err("Invalid J2 material parameters.");
        }
        if !(-1.0 < self.poisson_ratio && self.poisson_ratio < 0.5) {
            return err("Invalid Poisson ratio.");
        }
        if self.damage_onset < 0.0 || self.damage_rate < 0.0 {
            return err("Damage parameters must be non-negative.");
        }
        Ok(())
    }


    pub fn update<S: Scalar>(
        &self,
        strain: &Tensor<S>,
        plastic_strain: &Tensor<S>,
        eq_plastic_strain: &Tensor<S>,
        damage: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        self.validate()?;
        if strain.shape() != [6]
            || plastic_strain.shape() != [6]
            || eq_plastic_strain.ndim() != 0
            || damage.ndim() != 0
        {
            return err(
                "J2 requires two six-component strain vectors and scalar equivalent plastic strain and damage.",
            );
        }
        for v in [strain, plastic_strain, eq_plastic_strain, damage] {
            require_finite(v, "J2 states must be finite.")?;
        }
        let p = eq_plastic_strain.at(0);
        let d = damage.at(0);
        if p.value() < 0.0 {
            return err("Equivalent plastic strain must be non-negative.");
        }
        if !(0.0..=1.0).contains(&d.value()) {
            return err("Damage must lie in [0,1].");
        }
        let (e_mod, nu) = (self.youngs_modulus, self.poisson_ratio);
        let g = e_mod / (2.0 * (1.0 + nu));
        let k = e_mod / (3.0 * (1.0 - 2.0 * nu));
        let h = self.hardening_modulus;
        let e: [S; 6] = std::array::from_fn(|i| strain.at(i));
        let ep: [S; 6] = std::array::from_fn(|i| plastic_strain.at(i));
        let ee: [S; 6] = std::array::from_fn(|i| e[i] - ep[i]);
        let s = dev6(&ee).map(|x| x * (2.0 * g));
        let ss: Vec<S> = s.iter().map(|&x| x * x).collect();
        let q = (crate::array::pairwise_sum(&ss) * 1.5 + 1e-30).sqrt();
        let f = q - (p * h + self.yield_stress);
        let dg = softplus(f, self.transition_width) / (3.0 * g + h);
        let direction = s.map(|x| x * 1.5 / (q + 1e-30));
        let epn: [S; 6] = std::array::from_fn(|i| ep[i] + dg * direction[i]);
        let pn = p + dg;
        let damage_width = self.transition_width / self.yield_stress.max(1.0);
        let drive =
            softplus(pn - self.damage_onset, damage_width) - softplus(p - self.damage_onset, damage_width);
        let dn = -((-d + 1.0) * (drive * (-self.damage_rate)).exp()) + 1.0;
        let een: [S; 6] = std::array::from_fn(|i| e[i] - epn[i]);
        let trn = een[0] + een[1] + een[2];
        let sn = dev6(&een).map(|x| x * (2.0 * g));
        let volumetric = [1.0, 1.0, 1.0, 0.0, 0.0, 0.0];
        let stress: Vec<S> = (0..6).map(|i| (-dn + 1.0) * (trn * k * volumetric[i] + sn[i])).collect();
        let hardening_storage = (p * dg + dg * dg * 0.5) * h;
        let work = dg * self.yield_stress + hardening_storage;
        let mut out = ModelOutputs::new()
            .with("stress", Tensor::vector(stress))
            .with("plastic_strain", Tensor::vector(epn.to_vec()))
            .with_scalar("equivalent_plastic_strain", pn)
            .with_scalar("damage", dn)
            .with_scalar("input_state_margin", p.minimum(d.minimum(-d + 1.0)))
            .with_scalar("plastic_work_estimate", work)
            .with_scalar("hardening_storage_increment", hardening_storage);
        out.set_text("dissipation_status", J2_DISSIPATION_STATUS);
        out.set_scalar("yield_function", f);
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DissipationHeatConversion {
    pub taylor_quinney: f64,
    numeric_ok: bool,
}

impl RuntimeModel for DissipationHeatConversion {
    const CLASS: &'static str = "DissipationHeatConversion";
    const PARAMS: &'static [(&'static str, bool)] = &[("taylor_quinney", false)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        let numeric_ok = kw.raw("taylor_quinney").is_some_and(Value::is_number);
        Ok(Self { taylor_quinney: kw.f64("taylor_quinney", None).unwrap_or(f64::NAN), numeric_ok })
    }
}

impl DissipationHeatConversion {
    #[must_use]
    pub fn new(taylor_quinney: f64) -> Self {
        Self { taylor_quinney, numeric_ok: true }
    }


    pub fn validate(&self) -> PhysicsResult<()> {
        if !self.numeric_ok || !self.taylor_quinney.is_finite() || !(0.0..=1.0).contains(&self.taylor_quinney)
        {
            return err("Taylor-Quinney fraction must be a finite real scalar between zero and one.");
        }
        Ok(())
    }


    pub fn heat_rate<S: Scalar>(
        &self,
        dissipation: &Tensor<S>,
        dt: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        self.validate()?;
        if dt.ndim() > 0 && dt.shape() != dissipation.shape() {
            return err("Time increment must be scalar or match dissipation points.");
        }
        let q = dissipation.zip(dt, |d, t| d * self.taylor_quinney / t)?;
        Ok(ModelOutputs::new()
            .with("volumetric_heat_source", q)
            .with_scalar("time_step_margin_s", dt.min()?)
            .with_scalar("dissipation_margin", dissipation.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReactionNetwork {
    pub stoichiometry: Value,
    pub activation_energy: Value,
    pub prefactor: Value,
    pub reaction_enthalpy: Value,
    pub element_matrix: Option<Value>,
}

impl RuntimeModel for ReactionNetwork {
    const CLASS: &'static str = "ReactionNetwork";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("stoichiometry", false),
        ("activation_energy", false),
        ("prefactor", false),
        ("reaction_enthalpy", false),
        ("element_matrix", true),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        let raw = |n: &str| kw.raw(n).cloned().unwrap_or(Value::Null);
        Ok(Self {
            stoichiometry: raw("stoichiometry"),
            activation_energy: raw("activation_energy"),
            prefactor: raw("prefactor"),
            reaction_enthalpy: raw("reaction_enthalpy"),
            element_matrix: kw.any("element_matrix"),
        })
    }
}

struct Reactions {
    nu: Tensor<f64>,
    ea: Vec<f64>,
    a: Vec<f64>,
    dh: Vec<f64>,
}

impl ReactionNetwork {
    fn checked(&self) -> PhysicsResult<Reactions> {
        let arr = |v: &Value| Tensor::from_json(v);
        let nu = arr(&self.stoichiometry)?;
        let ea = arr(&self.activation_energy)?;
        let a = arr(&self.prefactor)?;
        let dh = arr(&self.reaction_enthalpy)?;
        if nu.ndim() != 2 || nu.shape().contains(&0) {
            return err("Stoichiometry must be nonempty species-by-reaction.");
        }
        let nr = nu.shape()[1];
        if ea.shape() != [nr] || a.shape() != [nr] || dh.shape() != [nr] {
            return err("Reaction parameter arrays must match the reaction count.");
        }
        let finite = |t: &Tensor<f64>| t.data().iter().all(Scalar::is_finite);
        if ea.data().iter().any(|v| *v < 0.0)
            || a.data().iter().any(|v| *v < 0.0)
            || !finite(&nu)
            || !finite(&ea)
            || !finite(&a)
            || !finite(&dh)
        {
            return err("Reaction data must be finite; activation energies and prefactors non-negative.");
        }
        if let Some(em) = &self.element_matrix {
            let elements = arr(em)?;
            let ns = nu.shape()[0];
            if elements.ndim() != 2
                || elements.shape()[0] == 0
                || elements.shape()[1] != ns
                || !finite(&elements)
                || elements.data().iter().any(|v| *v < 0.0)
            {
                return err("Element matrix must contain finite nonnegative counts for all species.");
            }
            let ne = elements.shape()[0];
            let mut scale: f64 = 1.0;
            let mut worst: f64 = 0.0;
            let mut all_ok = true;
            for e in 0..ne {
                for r in 0..nr {
                    let (mut bal, mut abs) = (0.0, 0.0);
                    for s in 0..ns {
                        bal += elements.at(e * ns + s) * nu.at(s * nr + r);
                        abs += elements.at(e * ns + s) * nu.at(s * nr + r).abs();
                    }
                    all_ok &= bal.is_finite();
                    worst = worst.max(bal.abs());
                    scale = scale.max(abs);
                }
            }
            if !scale.is_finite() || !all_ok || worst > 1e-12 * scale {
                return err("Authored stoichiometry violates finite element conservation.");
            }
        }
        Ok(Reactions { nu, ea: ea.into_data(), a: a.into_data(), dh: dh.into_data() })
    }


    pub fn validate(&self) -> PhysicsResult<()> {
        self.checked().map(|_| ())
    }


    pub fn rates<S: Scalar>(
        &self,
        concentration: &Tensor<S>,
        t: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        let r = self.checked()?;
        let (ns, nr) = (r.nu.shape()[0], r.nu.shape()[1]);
        if concentration.shape() != [ns] || t.ndim() != 0 {
            return err("Reaction network requires one species vector and scalar temperature.");
        }
        let temp = t.at(0);
        let mut rates = Vec::with_capacity(nr);
        for j in 0..nr {
            let mut product = S::one();
            let mut first = true;
            for s in 0..ns {
                let order = (-r.nu.at(s * nr + j)).max(0.0);
                if order > 0.0 {
                    let factor = concentration.at(s).powf(order);
                    product = if first { factor } else { product * factor };
                    first = false;
                }
            }
            let k = (S::from_f64(-r.ea[j]) / (temp * R_GAS)).exp() * r.a[j];
            rates.push(k * product);
        }
        let source: Vec<S> = (0..ns)
            .map(|s| {
                let mut acc = S::zero();
                for (j, &rate) in rates.iter().enumerate() {
                    acc += rate * r.nu.at(s * nr + j);
                }
                acc
            })
            .collect();
        let heat_terms: Vec<S> = rates.iter().zip(&r.dh).map(|(&x, &h)| x * h).collect();
        let heat = -crate::array::pairwise_sum(&heat_terms);
        Ok(ModelOutputs::new()
            .with("reaction_rate", Tensor::vector(rates))
            .with("species_source", Tensor::vector(source))
            .with_scalar("reaction_heat", heat)
            .with_scalar("concentration_margin", concentration.min()?)
            .with_scalar("temperature_margin_K", temp))
    }


    pub fn conservation_residual(&self) -> PhysicsResult<f64> {
        let Some(em) = &self.element_matrix else { return Ok(0.0) };
        let elements = Tensor::from_json(em)?;
        let nu = Tensor::from_json(&self.stoichiometry)?;
        if elements.ndim() != 2 || nu.ndim() != 2 || elements.shape()[1] != nu.shape()[0] {
            return err("matmul: Input operand 1 has a mismatch in its core dimension 0");
        }
        let (ne, ns, nr) = (elements.shape()[0], nu.shape()[0], nu.shape()[1]);
        let mut worst: f64 = 0.0;
        for e in 0..ne {
            for r in 0..nr {
                let bal: f64 = (0..ns).map(|s| elements.at(e * ns + s) * nu.at(s * nr + r)).sum();
                worst = worst.max(bal.abs());
            }
        }
        Ok(worst)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ButlerVolmerInterface {
    pub exchange_current_density: f64,
    pub alpha_anodic: f64,
    pub alpha_cathodic: f64,
    pub electrons: f64,
    pub equilibrium_potential: f64,
    pub entropy_change: f64,
    pub input_mode: String,
}

impl RuntimeModel for ButlerVolmerInterface {
    const CLASS: &'static str = "ButlerVolmerInterface";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("exchange_current_density", false),
        ("alpha_anodic", false),
        ("alpha_cathodic", false),
        ("electrons", false),
        ("equilibrium_potential", false),
        ("entropy_change", true),
        ("input_mode", true),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            exchange_current_density: kw.f64("exchange_current_density", None)?,
            alpha_anodic: kw.f64("alpha_anodic", None)?,
            alpha_cathodic: kw.f64("alpha_cathodic", None)?,
            electrons: kw.f64("electrons", None)?,
            equilibrium_potential: kw.f64("equilibrium_potential", None)?,
            entropy_change: kw.f64("entropy_change", Some(0.0))?,
            input_mode: kw.text("input_mode", Some("overpotential"))?,
        })
    }
}

impl ButlerVolmerInterface {

    pub fn validate(&self) -> PhysicsResult<()> {
        let v = [
            self.exchange_current_density,
            self.alpha_anodic,
            self.alpha_cathodic,
            self.electrons,
            self.equilibrium_potential,
            self.entropy_change,
        ];
        if !all_finite(&v) {
            return err("Butler-Volmer parameters must be finite.");
        }
        if self.exchange_current_density <= 0.0 || self.electrons <= 0.0 {
            return err("Exchange current and electron number must be positive.");
        }
        if self.alpha_anodic <= 0.0 || self.alpha_cathodic <= 0.0 {
            return err("Transfer coefficients must be positive.");
        }
        if self.input_mode != "overpotential" && self.input_mode != "electrode_potential" {
            return err("Unknown electrochemical input mode.");
        }
        Ok(())
    }


    pub fn response<S: Scalar>(
        &self,
        driving_voltage: &Tensor<S>,
        t: &Tensor<S>,
        activity_ratio: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        self.validate()?;
        let nf = self.electrons * FARADAY;
        let nernst =
            t.zip(activity_ratio, |tt, ratio| tt * R_GAS / nf * ratio.ln() + self.equilibrium_potential)?;
        let eta = if self.input_mode == "overpotential" {
            driving_voltage.clone()
        } else {
            driving_voltage.sub(&nernst)?
        };
        let ca = self.alpha_anodic * self.electrons * FARADAY;
        let cc = self.alpha_cathodic * self.electrons * FARADAY;
        let aa = t.map(|tt| S::from_f64(ca) / (tt * R_GAS));
        let ac = t.map(|tt| S::from_f64(cc) / (tt * R_GAS));
        let pos = aa.mul(&eta)?.map(Scalar::exp);
        let neg = ac.mul(&eta)?.map(|x| (-x).exp());
        let j = pos.sub(&neg)?.scale(self.exchange_current_density);
        let irreversible = j.mul(&eta)?;
        let reversible = j.mul(t)?.map(|x| -x * self.entropy_change / nf);
        let heat = irreversible.add(&reversible)?;
        Ok(ModelOutputs::new()
            .with("current_density", j)
            .with("reaction_heat", heat)
            .with("irreversible_heat", irreversible)
            .with("reversible_heat", reversible)
            .with("overpotential", eta)
            .with("equilibrium_potential", nernst)
            .with_scalar("activity_margin", activity_ratio.min()?)
            .with_scalar("temperature_margin_K", t.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseChangeEnthalpy {
    pub melting_temperature: f64,
    pub transition_width: f64,
    pub heat_capacity_solid: f64,
    pub heat_capacity_liquid: f64,
    pub latent_heat: f64,
    pub t_ref: f64,
}

impl RuntimeModel for PhaseChangeEnthalpy {
    const CLASS: &'static str = "PhaseChangeEnthalpy";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("melting_temperature", false),
        ("transition_width", false),
        ("heat_capacity_solid", false),
        ("heat_capacity_liquid", false),
        ("latent_heat", false),
        ("T_ref", false),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            melting_temperature: kw.f64("melting_temperature", None)?,
            transition_width: kw.f64("transition_width", None)?,
            heat_capacity_solid: kw.f64("heat_capacity_solid", None)?,
            heat_capacity_liquid: kw.f64("heat_capacity_liquid", None)?,
            latent_heat: kw.f64("latent_heat", None)?,
            t_ref: kw.f64("T_ref", None)?,
        })
    }
}

impl PhaseChangeEnthalpy {

    pub fn validate(&self) -> PhysicsResult<()> {
        let v = [
            self.melting_temperature,
            self.transition_width,
            self.heat_capacity_solid,
            self.heat_capacity_liquid,
            self.latent_heat,
            self.t_ref,
        ];
        if !all_finite(&v) || self.t_ref <= 0.0 {
            return err("Phase-change coefficients must be finite and reference temperature positive.");
        }
        if self.melting_temperature <= 0.0
            || self.transition_width <= 0.0
            || self.heat_capacity_solid <= 0.0
            || self.heat_capacity_liquid <= 0.0
            || self.latent_heat < 0.0
        {
            return err("Invalid phase-change parameters.");
        }
        Ok(())
    }


    pub fn response<S: Scalar>(&self, t: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        self.validate()?;
        let w = self.transition_width;
        let x0 = (self.t_ref - self.melting_temperature) / w;
        let dcp = self.heat_capacity_liquid - self.heat_capacity_solid;
        let x = t.map(|tt| (tt - self.melting_temperature) / w);
        let f = x.map(sigmoid);
        let sp0 = softplus(S::from_f64(x0), 1.0);
        let h = t.zip(&x, |tt, xx| {
            let integral = (softplus(xx, 1.0) - sp0) * w;
            (tt - self.t_ref) * self.heat_capacity_solid + integral * dcp + sigmoid(xx) * self.latent_heat
        })?;
        let cp = x.map(|xx| {
            let df = ((xx * 0.5).cosh().powi(2)).recip() * 0.25 / w;
            sigmoid(xx) * dcp + self.heat_capacity_solid + df * self.latent_heat
        });
        Ok(ModelOutputs::new()
            .with("phase_fraction", f)
            .with("specific_enthalpy", h)
            .with("effective_heat_capacity", cp)
            .with_scalar("temperature_margin_K", t.min()?))
    }
}


#[derive(Debug, Clone, PartialEq)]
pub struct VibroAcousticInterface {
    pub velocity_mapping: Value,
    pub quadrature_areas_m2: Option<Value>,
}

impl RuntimeModel for VibroAcousticInterface {
    const CLASS: &'static str = "VibroAcousticInterface";
    const PARAMS: &'static [(&'static str, bool)] =
        &[("velocity_mapping", false), ("quadrature_areas_m2", true)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            velocity_mapping: kw.raw("velocity_mapping").cloned().unwrap_or(Value::Null),
            quadrature_areas_m2: kw.any("quadrature_areas_m2"),
        })
    }
}

impl VibroAcousticInterface {

    pub fn response<S: Scalar>(&self, vs: &Tensor<S>, p: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        let Some(areas) = &self.quadrature_areas_m2 else {
            return err("Acoustic pressure transfer requires explicit quadrature areas in m^2.");
        };
        let bad_areas = "Acoustic quadrature areas must be positive finite real values.";
        let a = Tensor::from_json(areas).map_err(|_| PhysicsError::value(bad_areas))?;
        let bad_map = "Velocity mapping must have one finite row per acoustic quadrature point.";
        let b = Tensor::from_json(&self.velocity_mapping).map_err(|_| PhysicsError::value(bad_map))?;
        if a.ndim() != 1 || a.size() == 0 || !a.data().iter().all(|v| v.is_finite() && *v > 0.0) {
            return err(bad_areas);
        }
        if b.ndim() != 2
            || b.shape()[0] != a.size()
            || b.shape()[1] == 0
            || !b.data().iter().all(Scalar::is_finite)
        {
            return err(bad_map);
        }
        let (np_, nd) = (b.shape()[0], b.shape()[1]);
        if vs.shape() != [nd] || p.shape() != [np_] {
            return err("Velocity and pressure vectors must match the interface mapping.");
        }
        let weighted: Vec<S> = (0..np_).map(|i| p.at(i) * a.at(i)).collect();
        let vn: Vec<S> = (0..np_)
            .map(|i| {
                let mut acc = S::zero();
                for j in 0..nd {
                    acc += vs.at(j) * b.at(i * nd + j);
                }
                acc
            })
            .collect();
        let force: Vec<S> = (0..nd)
            .map(|j| {
                let mut acc = S::zero();
                for (i, &w) in weighted.iter().enumerate() {
                    acc += w * b.at(i * nd + j);
                }
                -acc
            })
            .collect();
        let pa =
            crate::array::pairwise_sum(&weighted.iter().zip(&vn).map(|(&w, &v)| w * v).collect::<Vec<_>>());
        let ps = crate::array::pairwise_sum(
            &force.iter().zip(vs.data()).map(|(&f, &v)| f * v).collect::<Vec<_>>(),
        );
        Ok(ModelOutputs::new()
            .with("normal_velocity", Tensor::vector(vn))
            .with("structural_force", Tensor::vector(force))
            .with_scalar("interface_power_residual", pa + ps))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalisedMovingHeatSource {
    pub absorbed_power: f64,
    pub radius: f64,
}

impl RuntimeModel for NormalisedMovingHeatSource {
    const CLASS: &'static str = "NormalisedMovingHeatSource";
    const PARAMS: &'static [(&'static str, bool)] = &[("absorbed_power", false), ("radius", false)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self { absorbed_power: kw.f64("absorbed_power", None)?, radius: kw.f64("radius", None)? })
    }
}

impl NormalisedMovingHeatSource {

    pub fn validate(&self) -> PhysicsResult<()> {
        if !self.absorbed_power.is_finite() || !self.radius.is_finite() {
            return err("Heat-source parameters must be finite.");
        }
        if self.absorbed_power < 0.0 || self.radius <= 0.0 {
            return err("Absorbed power must be non-negative and radius positive.");
        }
        Ok(())
    }


    pub fn volumetric_source<S: Scalar>(
        &self,
        points: &Tensor<S>,
        centre: &Tensor<S>,
        volumes: &Tensor<S>,
    ) -> PhysicsResult<ModelOutputs<S>> {
        self.validate()?;
        if points.ndim() != 2
            || centre.shape() != [points.shape()[1]]
            || volumes.shape() != [points.shape()[0]]
            || points.shape()[0] == 0
        {
            return err("Heat source requires nonempty points, centre vector and matching cell volumes.");
        }
        let r2 = 2.0 * self.radius * self.radius;
        let log_weight = points.sub(centre)?.map(|d| d * d).sum_last()?.map(|s| -s / r2);
        let top = log_weight.max()?;
        let raw = log_weight.map(|l| (l - top).exp());
        let norm = raw.mul(volumes)?.sum();
        let q = raw.map(|r| r * self.absorbed_power / norm);
        let integrated = q.mul(volumes)?.sum();
        Ok(ModelOutputs::new()
            .with("volumetric_heat_source", q)
            .with_scalar("integrated_power", integrated)
            .with_scalar("cell_volume_margin", volumes.min()?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopologyScalarInterpolation {
    pub solid_value: f64,
    pub void_value: f64,
    pub penalty: PyNum,
}

impl RuntimeModel for TopologyScalarInterpolation {
    const CLASS: &'static str = "TopologyScalarInterpolation";
    const PARAMS: &'static [(&'static str, bool)] =
        &[("solid_value", false), ("void_value", false), ("penalty", true)];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        Ok(Self {
            solid_value: kw.f64("solid_value", None)?,
            void_value: kw.f64("void_value", None)?,
            penalty: kw.num("penalty", Some(PyNum::Float(3.0)))?,
        })
    }
}

impl TopologyScalarInterpolation {

    pub fn response<S: Scalar>(
        &self,
        rho: &Tensor<S>,
        quantity_name: &str,
    ) -> PhysicsResult<ModelOutputs<S>> {
        if !all_finite(&[self.solid_value, self.void_value, self.penalty.as_f64()])
            || self.penalty.as_f64() < 1.0
        {
            return err("Interpolation endpoints must be finite and penalty at least one.");
        }
        let value = rho.map(|r| {
            pow_num(r.clip(0.0, 1.0), self.penalty) * (self.solid_value - self.void_value) + self.void_value
        });
        Ok(ModelOutputs::new().with(quantity_name, value))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopologyMaterialInterpolation {
    pub pairs: [(f64, f64); 6],
    pub penalty: PyNum,
}

const MATERIAL_PAIRS: [(&str, &str, &str); 6] = [
    ("solid_electrical_conductivity", "void_electrical_conductivity", "electrical_conductivity"),
    ("solid_ionic_diffusivity", "void_ionic_diffusivity", "ionic_diffusivity"),
    ("solid_permeability", "void_permeability", "permeability"),
    ("solid_thermal_conductivity", "void_thermal_conductivity", "thermal_conductivity"),
    ("solid_youngs_modulus", "void_youngs_modulus", "youngs_modulus"),
    ("solid_density", "void_density", "mass_density"),
];

impl RuntimeModel for TopologyMaterialInterpolation {
    const CLASS: &'static str = "TopologyMaterialInterpolation";
    const PARAMS: &'static [(&'static str, bool)] = &[
        ("solid_electrical_conductivity", false),
        ("void_electrical_conductivity", false),
        ("solid_ionic_diffusivity", false),
        ("void_ionic_diffusivity", false),
        ("solid_permeability", false),
        ("void_permeability", false),
        ("solid_thermal_conductivity", false),
        ("void_thermal_conductivity", false),
        ("solid_youngs_modulus", false),
        ("void_youngs_modulus", false),
        ("solid_density", false),
        ("void_density", false),
        ("penalty", true),
    ];
    fn build(kw: &Kwargs<'_>) -> PhysicsResult<Self> {
        let mut pairs = [(0.0, 0.0); 6];
        for (slot, (s, v, _)) in pairs.iter_mut().zip(MATERIAL_PAIRS) {
            *slot = (kw.f64(s, None)?, kw.f64(v, None)?);
        }
        Ok(Self { pairs, penalty: kw.num("penalty", Some(PyNum::Float(3.0)))? })
    }
}

impl TopologyMaterialInterpolation {

    pub fn response<S: Scalar>(&self, rho: &Tensor<S>) -> PhysicsResult<ModelOutputs<S>> {
        let ok = self.pairs.iter().all(|(s, v)| s.is_finite() && *s >= 0.0 && v.is_finite() && *v >= 0.0)
            && self.penalty.is_finite()
            && self.penalty.as_f64() >= 0.0;
        if !ok || self.penalty.as_f64() < 1.0 {
            return err(
                "Material interpolation requires finite nonnegative properties and penalty at least one.",
            );
        }
        let w = rho.map(|r| pow_num(r.clip(0.0, 1.0), self.penalty));
        let mut out = ModelOutputs::new();
        for ((solid, void), (_, _, name)) in self.pairs.iter().zip(MATERIAL_PAIRS) {
            out.set(name, w.map(|x| x * (solid - void) + *void));
        }
        Ok(out)
    }
}
